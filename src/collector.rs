//! Ciclo de coleta: amostra leve a cada 5 s e coleta completa a cada minuto.

use crate::config::Config;
use crate::model::{Live, Sample};
use crate::platform::{self, Sampler};
use crate::model::State;
use crate::{checks, insights, ssh, store, util};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

pub const LIVE_EVERY: Duration = Duration::from_secs(5);
pub const FULL_EVERY: Duration = Duration::from_secs(60);
const ACARI_EVERY: i64 = 15 * 60;

/// Últimos documentos prontos para a página. Ficam só na memória.
#[derive(Default)]
pub struct Documents {
    pub live: RwLock<Option<Vec<u8>>>,
    pub current: RwLock<Option<Vec<u8>>>,
}

struct Job {
    now: i64,
    sample: Sample,
    rates: (Option<f64>, Option<f64>),
}

pub fn live(now: i64, sample: Sample, rates: (Option<f64>, Option<f64>)) -> Live {
    Live { generated_at: now, rx_bps: rates.0, tx_bps: rates.1, sample }
}

fn network(gateway: Option<&str>) -> Value {
    let gateway_ok = gateway.is_some_and(platform::ping);
    let wan_ok = util::probe_tcp("1.1.1.1", 443) || platform::ping("1.1.1.1");
    let dns_ok = util::probe_dns("one.one.one.one");
    let status = match (gateway_ok, wan_ok, dns_ok) {
        (_, true, true) => "ok",
        (_, true, false) => "sem_dns",
        (true, false, _) => "sem_internet",
        _ => "sem_rede",
    };
    json!({"status": status, "gateway": gateway, "gateway_ok": gateway_ok, "wan_ok": wan_ok, "dns_ok": dns_ok})
}

fn acari(conn: &Connection, now: i64, path: Option<&Path>) -> Value {
    let cached = store::meta_get(conn, "acari");
    let stamp = store::meta_get(conn, "acari_ts").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    if let Some(cached) = cached.filter(|_| now - stamp < ACARI_EVERY) {
        return serde_json::from_str(&cached).unwrap_or(Value::Null);
    }
    let Some(path) = path.filter(|path| path.exists()) else { return Value::Null };
    let Some(output) = util::run(&path.to_string_lossy(), &["check", "--json"], Duration::from_secs(30)) else {
        return Value::Null;
    };
    let Ok(payload) = serde_json::from_str::<Value>(&output) else { return Value::Null };
    let view = json!({
        "level": payload.pointer("/status/level"),
        "regenerable_bytes": payload.get("regenerable"),
        "highlights": payload.get("highlights").cloned().unwrap_or(json!([])),
        "report_age_days": payload.get("report_age_days"),
    });
    let _ = store::meta_set(conn, "acari", &view.to_string());
    let _ = store::meta_set(conn, "acari_ts", &now.to_string());
    view
}

/// Monta o documento completo. Lento (journal, ping, ss, ps): roda fora do ciclo de 5 s.
pub fn full_document(conn: &Connection, config: &Config, now: i64, sample: Sample, rates: (Option<f64>, Option<f64>)) -> rusqlite::Result<Value> {
    let (events, ssh_error) = platform::read_ssh(conn);
    let recorded = store::record(conn, now, &sample, &events)?;
    let rates = (rates.0.or(recorded.0), rates.1.or(recorded.1));
    let mut ssh_view = serde_json::to_value(store::ssh_view(conn, now)?).unwrap_or(Value::Null);
    ssh_view["sessions"] = json!(ssh::parse_who(&util::run("who", &[], Duration::from_secs(5)).unwrap_or_default()));
    ssh_view["error"] = json!(ssh_error);
    let mut posture = platform::posture(config);
    let listeners = platform::listeners();
    posture.extend(checks::service_checks(&config.services, &listeners));
    let extras = platform::extras(conn, now, &sample, config);
    let memory_labels: Vec<String> = sample
        .memory
        .as_ref()
        .map(|memory| memory.parts.iter().take(3).map(|part| part.label.clone()).collect())
        .unwrap_or_default();
    let network = network(sample.gateway.as_deref());
    let (seen_listeners, listeners_since) = store::record_listeners(conn, now, &listeners)?;
    let ssh_facts = store::ssh_facts(conn, now)?;
    let key_labels = platform::key_labels();
    let failed_units = platform::failed_units();
    let oom_kills = platform::oom_kills();
    let password_login = posture.iter().find(|item| item.id == "ssh_senha").and_then(|item| match item.state {
        State::Ok => Some(false),
        State::Problema => Some(true),
        _ => None,
    });
    let facts = insights::Facts {
        now,
        ssh: &ssh_facts,
        key_labels: &key_labels,
        password_login,
        fail2ban: extras.fail2ban.as_ref(),
        failed_units: failed_units.as_deref(),
        oom_kills: oom_kills.as_deref(),
        listeners: &seen_listeners,
        listeners_since,
        devices: extras.devices.as_deref(),
    };
    let readings = insights::readings(&facts);
    ssh_view["who"] = json!(insights::who(&facts));
    let mut document = json!({
        "generated_at": now,
        "platform": platform::NAME,
        "host": platform::host_info(config),
        "memory_labels": memory_labels,
        "series": {
            "hour": store::series(conn, now, 3600, 60)?,
            "day": store::series(conn, now, 24 * 3600, 900)?,
        },
        "ssh": ssh_view,
        "posture": posture,
        "network": network,
        "listeners": listeners,
        "readings": readings,
        "processes": platform::processes(),
        "acari": acari(conn, now, config.acari.clone().or_else(platform::default_acari).as_deref()),
        "tailscale": {"state": platform::tailscale_state()},
        "now": live(now, sample, rates),
    });
    if let Some(fail2ban) = extras.fail2ban {
        document["fail2ban"] = json!(fail2ban);
    }
    if let Some(devices) = extras.devices {
        document["devices"] = json!(devices);
    }
    Ok(document)
}

fn spawn_full_worker(data: PathBuf, config: Arc<Config>, documents: Arc<Documents>) -> SyncSender<Job> {
    let (sender, receiver) = sync_channel::<Job>(0);
    std::thread::Builder::new()
        .name("coleta-completa".into())
        .spawn(move || {
            let conn = match store::open(&data.join("status.db")) {
                Ok(conn) => conn,
                Err(error) => {
                    eprintln!("não abri o banco em {}: {error}", data.display());
                    return;
                }
            };
            for job in receiver {
                match full_document(&conn, &config, job.now, job.sample, job.rates) {
                    Ok(document) => *documents.current.write().unwrap_or_else(|p| p.into_inner()) = Some(document.to_string().into_bytes()),
                    Err(error) => eprintln!("coleta completa falhou: {error}"),
                }
            }
        })
        .expect("thread da coleta completa");
    sender
}

/// Roda para sempre na thread chamadora.
pub fn run(data: PathBuf, config: Arc<Config>, documents: Arc<Documents>) {
    let full = spawn_full_worker(data, config, documents.clone());
    let mut sampler = Sampler::new();
    let mut previous: Option<(Instant, Option<u64>, Option<u64>)> = None;
    let mut next_full = Instant::now();
    loop {
        let started = Instant::now();
        let now = util::now();
        let sample = sampler.sample();
        let rates = match previous {
            Some((when, rx, tx)) => {
                let elapsed = when.elapsed().as_secs_f64();
                (store::rate_bps(rx, sample.rx_bytes, elapsed), store::rate_bps(tx, sample.tx_bytes, elapsed))
            }
            None => (None, None),
        };
        previous = Some((started, sample.rx_bytes, sample.tx_bytes));
        if let Ok(bytes) = serde_json::to_vec(&live(now, sample.clone(), rates)) {
            *documents.live.write().unwrap_or_else(|p| p.into_inner()) = Some(bytes);
        }
        if started >= next_full {
            // Canal sem buffer: se a coleta anterior ainda roda, esta é pulada.
            match full.try_send(Job { now, sample, rates }) {
                Ok(()) => next_full = started + FULL_EVERY,
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => eprintln!("a coleta completa parou"),
            }
        }
        std::thread::sleep(LIVE_EVERY.saturating_sub(started.elapsed()).max(Duration::from_millis(500)));
    }
}
