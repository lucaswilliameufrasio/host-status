//! Servidor da página: só GET/HEAD de arquivos embutidos e dos dois JSON.

use crate::collector::Documents;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

const INDEX: &str = include_str!("../www/index.html");
const SCRIPT: &str = include_str!("../www/app.js");
const STYLE: &str = include_str!("../www/app.css");
const MAX_CONNECTIONS: usize = 64;

fn respond(status: StatusCode, content_type: &str, body: Bytes, head: bool) -> Response<Full<Bytes>> {
    let length = body.len();
    let mut response = Response::new(Full::new(if head { Bytes::new() } else { body }));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("text/plain")));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'self'"));
    response
}

fn route(request: &Request<Incoming>, documents: &Documents) -> Response<Full<Bytes>> {
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let mut response = respond(StatusCode::METHOD_NOT_ALLOWED, "text/plain; charset=utf-8", Bytes::from_static(b"metodo nao permitido"), false);
        response.headers_mut().insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return response;
    }
    let json = |slot: &std::sync::RwLock<Option<Vec<u8>>>| match slot.read().unwrap_or_else(|p| p.into_inner()).clone() {
        Some(bytes) => respond(StatusCode::OK, "application/json", Bytes::from(bytes), head),
        None => respond(StatusCode::SERVICE_UNAVAILABLE, "text/plain; charset=utf-8", Bytes::from_static(b"primeira coleta ainda nao terminou"), head),
    };
    match request.uri().path() {
        "/" | "/index.html" => respond(StatusCode::OK, "text/html; charset=utf-8", Bytes::from_static(INDEX.as_bytes()), head),
        "/app.js" => respond(StatusCode::OK, "text/javascript; charset=utf-8", Bytes::from_static(SCRIPT.as_bytes()), head),
        "/app.css" => respond(StatusCode::OK, "text/css; charset=utf-8", Bytes::from_static(STYLE.as_bytes()), head),
        "/live.json" => json(&documents.live),
        "/current.json" => json(&documents.current),
        _ => respond(StatusCode::NOT_FOUND, "text/plain; charset=utf-8", Bytes::from_static(b"nao encontrado"), head),
    }
}

/// "127.0.0.1:8787" ou "tailscale:80". O nome "tailscale" vira o IP da Tailscale da máquina.
pub fn resolve(spec: &str) -> Option<SocketAddr> {
    let (host, port) = spec.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let ip: IpAddr = if host == "tailscale" {
        IpAddr::V4(crate::util::tailscale_ipv4()?)
    } else {
        host.trim_matches(|c| c == '[' || c == ']').parse().ok()?
    };
    Some(SocketAddr::new(ip, port))
}

async fn serve_one(listener: TcpListener, documents: Arc<Documents>, limit: Arc<Semaphore>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let Ok(permit) = limit.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let documents = documents.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request: Request<Incoming>| {
                let response = route(&request, &documents);
                async move { Ok::<_, Infallible>(response) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10))
                .max_buf_size(16 * 1024)
                .serve_connection(TokioIo::new(stream), service)
                .await;
            drop(permit);
        });
    }
}

/// Mantém um endereço no ar. Se o IP da Tailscale mudar ou sumir, refaz o bind.
pub async fn supervise(spec: String, documents: Arc<Documents>, limit: Arc<Semaphore>) {
    loop {
        let Some(address) = resolve(&spec) else {
            tokio::time::sleep(Duration::from_secs(30)).await;
            continue;
        };
        let listener = match TcpListener::bind(address).await {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("não abri {address} ({spec}): {error}");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };
        eprintln!("página em http://{address}/");
        let server = tokio::spawn(serve_one(listener, documents.clone(), limit.clone()));
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if resolve(&spec) != Some(address) {
                eprintln!("{spec} mudou de {address}; refazendo o bind");
                server.abort();
                break;
            }
        }
    }
}

pub fn serve(binds: Vec<String>, documents: Arc<Documents>) -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build()?;
    runtime.block_on(async move {
        let limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let tasks: Vec<_> = binds.into_iter().map(|spec| tokio::spawn(supervise(spec, documents.clone(), limit.clone()))).collect();
        for task in tasks {
            let _ = task.await;
        }
    });
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_bind_specs() {
        assert_eq!(resolve("127.0.0.1:8787"), Some("127.0.0.1:8787".parse().unwrap()));
        assert_eq!(resolve("[::1]:80"), Some("[::1]:80".parse().unwrap()));
        assert_eq!(resolve("sem-porta"), None);
    }
}
