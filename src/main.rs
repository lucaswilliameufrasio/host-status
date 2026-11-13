mod checks;
mod collector;
mod config;
mod http;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod macos;
mod model;
mod platform;
mod ssh;
mod store;
mod util;

use std::path::PathBuf;
use std::sync::Arc;

const USAGE: &str = "uso: host-status [--config ARQUIVO] [--data PASTA] [--once]

  --config  configuração do host (padrão: /etc/host-status/config.json, se existir)
  --data    pasta do banco (padrão: /var/lib/host-status como root, ~/.local/share/host-status sem root)
  --once    faz uma coleta completa, imprime o JSON e sai
  --check-config  só valida a configuração e sai";

fn main() {
    let mut config_path: Option<PathBuf> = None;
    let mut data: Option<PathBuf> = None;
    let mut once = false;
    let mut check_only = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => config_path = args.next().map(PathBuf::from),
            "--data" => data = args.next().map(PathBuf::from),
            "--once" => once = true,
            "--check-config" => check_only = true,
            "--diagnose" => {
                for line in platform::diagnose() {
                    println!("{line}");
                }
                return;
            }
            "--version" => {
                println!("host-status {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(if arg == "--help" || arg == "-h" { 0 } else { 2 });
            }
        }
    }
    let default_config = PathBuf::from("/etc/host-status/config.json");
    let config = match config_path.or_else(|| default_config.exists().then_some(default_config)) {
        Some(path) => config::load(&path).unwrap_or_else(|error| {
            eprintln!("configuração inválida: {error}");
            std::process::exit(2);
        }),
        None => config::Config::default(),
    };
    if check_only {
        for spec in &config.bind {
            if !spec.starts_with("tailscale:") && http::resolve(spec).is_none() {
                eprintln!("endereço inválido em bind: {spec}");
                std::process::exit(2);
            }
        }
        println!("configuração ok");
        return;
    }
    // SAFETY: geteuid só lê o id efetivo do processo.
    let root = unsafe { libc::geteuid() } == 0;
    let data = data.unwrap_or_else(|| {
        if root {
            PathBuf::from("/var/lib/host-status")
        } else {
            let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
            if cfg!(target_os = "macos") {
                home.join("Library/Application Support/host-status")
            } else {
                home.join(".local/share/host-status")
            }
        }
    });
    if let Err(error) = std::fs::create_dir_all(&data) {
        eprintln!("não criei {}: {error}", data.display());
        std::process::exit(1);
    }
    let config = Arc::new(config);
    if once {
        let conn = store::open(&data.join("status.db")).unwrap_or_else(|error| {
            eprintln!("banco: {error}");
            std::process::exit(1);
        });
        let mut sampler = platform::Sampler::new();
        let sample = sampler.sample();
        match collector::full_document(&conn, &config, util::now(), sample, (None, None)) {
            Ok(document) => println!("{}", serde_json::to_string_pretty(&document).unwrap_or_default()),
            Err(error) => {
                eprintln!("coleta falhou: {error}");
                std::process::exit(1);
            }
        }
        return;
    }
    let documents = Arc::new(collector::Documents::default());
    let binds = config.bind.clone();
    {
        let documents = documents.clone();
        let config = config.clone();
        std::thread::Builder::new()
            .name("coleta".into())
            .spawn(move || collector::run(data, config, documents))
            .expect("thread de coleta");
    }
    if let Err(error) = http::serve(binds, documents) {
        eprintln!("servidor: {error}");
        std::process::exit(1);
    }
}
