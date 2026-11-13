//! Histórico em SQLite: amostras por minuto, eventos de SSH e aparelhos vistos na rede.

use crate::model::{Neighbor, Sample, SshEvent};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;

const SAMPLE_RETENTION: i64 = 30 * 24 * 3600;
const EVENT_RETENTION: i64 = 90 * 24 * 3600;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const DEVICE_RETENTION: i64 = 180 * 24 * 3600;
const LISTENER_RETENTION: i64 = 30 * 24 * 3600;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    prepare(&conn)?;
    Ok(conn)
}

pub fn prepare(conn: &Connection) -> rusqlite::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS samples (
            ts INTEGER PRIMARY KEY,
            cpu_pct REAL, load1 REAL, mem_used_pct REAL, swap_used_mb REAL,
            disk_used_pct REAL, disk_free_bytes INTEGER,
            rx_bytes INTEGER, tx_bytes INTEGER, rx_bps REAL, tx_bps REAL,
            mem_p1_bytes INTEGER, mem_p2_bytes INTEGER, mem_p3_bytes INTEGER
        );
        CREATE TABLE IF NOT EXISTS ssh_events (
            uid TEXT PRIMARY KEY, ts INTEGER NOT NULL,
            kind TEXT NOT NULL, method TEXT, user TEXT, ip TEXT, key TEXT
        );
        CREATE INDEX IF NOT EXISTS ssh_events_ts ON ssh_events (ts);
        CREATE INDEX IF NOT EXISTS ssh_events_kind ON ssh_events (kind, ts);
        CREATE TABLE IF NOT EXISTS devices (
            mac TEXT PRIMARY KEY, ip TEXT, vendor TEXT, label TEXT,
            first_seen INTEGER, last_seen INTEGER, last_state TEXT
        );
        CREATE TABLE IF NOT EXISTS listeners_seen (
            port INTEGER NOT NULL, address TEXT NOT NULL, process TEXT,
            first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
            PRIMARY KEY (port, address)
        );
        CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT);",
    )
}

pub fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0))
        .optional()
        .ok()
        .flatten()
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn rate_bps(previous: Option<u64>, current: Option<u64>, elapsed: f64) -> Option<f64> {
    let (previous, current) = (previous?, current?);
    if elapsed <= 0.0 || current < previous {
        return None;
    }
    Some(round((current - previous) as f64 * 8.0 / elapsed, 1))
}

pub fn round(value: f64, digits: i32) -> f64 {
    let factor = 10f64.powi(digits);
    (value * factor).round() / factor
}

/// Grava a amostra do minuto e os eventos novos. Devolve as taxas de rede desde a amostra anterior.
pub fn record(conn: &Connection, now: i64, sample: &Sample, events: &[SshEvent]) -> rusqlite::Result<(Option<f64>, Option<f64>)> {
    let previous: Option<(i64, Option<i64>, Option<i64>)> = conn
        .query_row("SELECT ts, rx_bytes, tx_bytes FROM samples ORDER BY ts DESC LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?;
    let (rx_bps, tx_bps) = match previous {
        Some((ts, rx, tx)) => {
            let elapsed = (now - ts) as f64;
            (
                rate_bps(rx.map(|v| v as u64), sample.rx_bytes, elapsed),
                rate_bps(tx.map(|v| v as u64), sample.tx_bytes, elapsed),
            )
        }
        None => (None, None),
    };
    let part = |index: usize| -> Option<i64> {
        sample.memory.as_ref().and_then(|memory| memory.parts.get(index)).map(|part| part.bytes as i64)
    };
    conn.execute(
        "INSERT OR REPLACE INTO samples (ts, cpu_pct, load1, mem_used_pct, swap_used_mb, disk_used_pct,
            disk_free_bytes, rx_bytes, tx_bytes, rx_bps, tx_bps, mem_p1_bytes, mem_p2_bytes, mem_p3_bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            now,
            sample.cpu_pct,
            sample.load1,
            sample.memory.as_ref().map(|memory| memory.used_pct),
            sample.swap.as_ref().map(|swap| swap.used_mb),
            sample.disk.as_ref().map(|disk| disk.used_pct),
            sample.disk.as_ref().map(|disk| disk.free_bytes as i64),
            sample.rx_bytes.map(|v| v as i64),
            sample.tx_bytes.map(|v| v as i64),
            rx_bps,
            tx_bps,
            part(0),
            part(1),
            part(2),
        ],
    )?;
    for event in events {
        conn.execute(
            "INSERT OR IGNORE INTO ssh_events VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![event.uid, event.ts, event.kind, event.method, event.user, event.ip, event.key],
        )?;
    }
    conn.execute("DELETE FROM samples WHERE ts < ?1", [now - SAMPLE_RETENTION])?;
    conn.execute("DELETE FROM ssh_events WHERE ts < ?1", [now - EVENT_RETENTION])?;
    Ok((rx_bps, tx_bps))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Point {
    pub ts: i64,
    pub cpu_pct: Option<f64>,
    pub mem_used_pct: Option<f64>,
    pub load1: Option<f64>,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    pub disk_used_pct: Option<f64>,
    pub mem_p1_bytes: Option<f64>,
    pub mem_p2_bytes: Option<f64>,
    pub mem_p3_bytes: Option<f64>,
}

/// Média por balde. Balde sem amostra fica de fora: a página mostra o buraco.
pub fn series(conn: &Connection, now: i64, seconds: i64, bucket: i64) -> rusqlite::Result<Vec<Point>> {
    let mut statement = conn.prepare_cached(
        "SELECT (ts / ?1) * ?1, AVG(cpu_pct), AVG(mem_used_pct), AVG(load1), AVG(rx_bps), AVG(tx_bps),
                AVG(disk_used_pct), AVG(mem_p1_bytes), AVG(mem_p2_bytes), AVG(mem_p3_bytes)
         FROM samples WHERE ts >= ?2 GROUP BY ts / ?1 ORDER BY 1",
    )?;
    let one = |value: Option<f64>, digits: i32| value.map(|v| round(v, digits));
    let rows = statement.query_map(params![bucket, now - seconds], |row| {
        Ok(Point {
            ts: row.get(0)?,
            cpu_pct: one(row.get(1)?, 1),
            mem_used_pct: one(row.get(2)?, 1),
            load1: one(row.get(3)?, 2),
            rx_bps: one(row.get(4)?, 1),
            tx_bps: one(row.get(5)?, 1),
            disk_used_pct: one(row.get(6)?, 1),
            mem_p1_bytes: one(row.get(7)?, 0),
            mem_p2_bytes: one(row.get(8)?, 0),
            mem_p3_bytes: one(row.get(9)?, 0),
        })
    })?;
    rows.collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct Origin {
    pub ip: String,
    pub accepted: i64,
    pub refused: i64,
    pub last_ts: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecentEvent {
    pub ts: i64,
    pub kind: String,
    pub method: String,
    pub user: String,
    pub ip: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SshView {
    pub recent: Vec<RecentEvent>,
    pub counts: BTreeMap<String, BTreeMap<String, i64>>,
    pub origins_7d: Vec<Origin>,
    pub since_ts: Option<i64>,
}

pub fn ssh_view(conn: &Connection, now: i64) -> rusqlite::Result<SshView> {
    let mut statement = conn.prepare_cached(
        "SELECT ts, kind, method, user, ip, key FROM ssh_events ORDER BY ts DESC LIMIT 300",
    )?;
    let recent = statement
        .query_map([], |row| {
            Ok(RecentEvent {
                ts: row.get(0)?,
                kind: row.get(1)?,
                method: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                user: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                ip: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                key: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts = BTreeMap::new();
    for (window, seconds) in [("24h", 24 * 3600), ("7d", 7 * 24 * 3600)] {
        let mut by_kind = BTreeMap::new();
        let mut statement = conn.prepare_cached("SELECT kind, COUNT(*) FROM ssh_events WHERE ts >= ?1 GROUP BY kind")?;
        for row in statement.query_map([now - seconds], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))? {
            let (kind, total) = row?;
            by_kind.insert(kind, total);
        }
        counts.insert(window.to_string(), by_kind);
    }
    let mut statement = conn.prepare_cached(
        "SELECT ip, SUM(kind = 'aceito'), SUM(kind != 'aceito'), MAX(ts) FROM ssh_events
         WHERE ts >= ?1 GROUP BY ip ORDER BY MAX(ts) DESC LIMIT 20",
    )?;
    let origins_7d = statement
        .query_map([now - 7 * 24 * 3600], |row| {
            Ok(Origin {
                ip: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                accepted: row.get(1)?,
                refused: row.get(2)?,
                last_ts: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let since_ts = conn.query_row("SELECT MIN(ts) FROM ssh_events", [], |row| row.get(0))?;
    Ok(SshView { recent, counts, origins_7d, since_ts })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Device {
    pub mac: String,
    pub ip: String,
    pub vendor: String,
    pub label: String,
    pub first_seen: i64,
    pub last_seen: i64,
    pub connected: bool,
}

/// Guarda a primeira e a última vez que cada aparelho apareceu.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn record_devices(conn: &Connection, now: i64, neighbors: &[Neighbor]) -> rusqlite::Result<()> {
    for item in neighbors {
        conn.execute(
            "INSERT INTO devices (mac, ip, vendor, label, first_seen, last_seen, last_state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)
             ON CONFLICT(mac) DO UPDATE SET ip = excluded.ip, vendor = excluded.vendor,
                 label = excluded.label, last_seen = excluded.last_seen, last_state = excluded.last_state",
            params![item.mac, item.ip, item.vendor, item.label, now, item.state],
        )?;
    }
    conn.execute("DELETE FROM devices WHERE last_seen < ?1", [now - DEVICE_RETENTION])?;
    Ok(())
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn devices_view(conn: &Connection, connected: &HashSet<String>) -> rusqlite::Result<Vec<Device>> {
    let mut statement = conn.prepare_cached(
        "SELECT mac, ip, vendor, label, first_seen, last_seen FROM devices ORDER BY last_seen DESC",
    )?;
    let rows = statement.query_map([], |row| {
        let mac: String = row.get(0)?;
        Ok(Device {
            connected: connected.contains(&mac),
            mac,
            ip: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            vendor: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            label: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            first_seen: row.get(4)?,
            last_seen: row.get(5)?,
        })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Memory, MemoryPart};

    fn memory() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        prepare(&conn).unwrap();
        conn
    }

    fn event(uid: &str, ts: i64, kind: &str) -> SshEvent {
        SshEvent {
            uid: uid.into(), ts, kind: kind.into(), method: "publickey".into(),
            user: "admin".into(), ip: "10.0.0.1".into(), key: "SHA256:x".into(),
        }
    }

    fn ssh(uid: &str, ts: i64, kind: &str, user: &str, ip: &str, method: &str, key: &str) -> SshEvent {
        SshEvent { uid: uid.into(), ts, kind: kind.into(), method: method.into(), user: user.into(), ip: ip.into(), key: key.into() }
    }

    #[test]
    fn facts_group_logins_and_attempts() {
        let conn = memory();
        let now = 10 * 24 * 3600;
        let events = vec![
            ssh("a", now - 9 * 24 * 3600, "aceito", "admin", "192.0.2.1", "publickey", "SHA256:k1"),
            ssh("b", now - 3600, "aceito", "admin", "192.0.2.1", "publickey", "SHA256:k1"),
            ssh("c", now - 1800, "aceito", "admin", "192.0.2.1", "publickey", "SHA256:k1"),
            ssh("d", now - 600, "aceito", "admin", "100.64.0.5", "publickey", "SHA256:k1"),
            ssh("e", now - 500, "aceito", "deploy", "192.0.2.9", "password", ""),
            ssh("f", now - 400, "usuario_invalido", "oracle", "203.0.113.7", "", ""),
            ssh("g", now - 300, "usuario_invalido", "oracle", "203.0.113.7", "", ""),
            ssh("h", now - 200, "falha", "root", "203.0.113.8", "password", ""),
            ssh("i", now - 2 * 24 * 3600, "falha", "root", "203.0.113.9", "password", ""),
        ];
        record(&conn, now, &Sample::default(), &events).unwrap();
        let facts = ssh_facts(&conn, now).unwrap();
        assert_eq!(facts.identities.len(), 2);
        let admin = facts.identities.iter().find(|item| item.user == "admin").unwrap();
        assert_eq!((admin.entries, admin.key.as_str()), (3, "SHA256:k1"));
        assert_eq!(admin.origins[0], OriginCount { ip: "192.0.2.1".into(), entries: 2, first_ts: now - 9 * 24 * 3600 });
        assert_eq!(admin.origins[1].ip, "100.64.0.5");
        assert_eq!(facts.identities.iter().find(|item| item.user == "deploy").unwrap().method, "password");
        assert_eq!(facts.attempts_24h.total, 3);
        assert_eq!(facts.attempts_24h.addresses, 2);
        assert_eq!(facts.attempts_24h.top_addresses[0], ("203.0.113.7".into(), 2));
        assert_eq!(facts.attempts_24h.top_users[0], ("oracle".into(), 2));
        assert_eq!(facts.attempts_24h.per_address.len(), 2);
        assert_eq!(facts.since_ts, Some(now - 9 * 24 * 3600));
    }

    #[test]
    fn listeners_keep_first_seen() {
        let conn = memory();
        let ssh_port = crate::model::Listener { port: 22, address: "todas as interfaces".into(), process: "sshd".into() };
        let db = crate::model::Listener { port: 5432, address: "todas as interfaces".into(), process: "postgres".into() };
        let (open, baseline) = record_listeners(&conn, 1000, std::slice::from_ref(&ssh_port)).unwrap();
        assert_eq!((open.len(), baseline), (1, 1000));
        let (open, baseline) = record_listeners(&conn, 2000, &[ssh_port, db]).unwrap();
        assert_eq!(baseline, 1000);
        assert_eq!(open.iter().map(|item| (item.port, item.first_seen)).collect::<Vec<_>>(), vec![(22, 1000), (5432, 2000)]);
    }

    #[test]
    fn rate() {
        assert_eq!(rate_bps(Some(1000), Some(2000), 60.0), Some(133.3));
        assert_eq!(rate_bps(Some(2000), Some(1000), 60.0), None);
        assert_eq!(rate_bps(None, Some(1000), 60.0), None);
    }

    #[test]
    fn record_rates_dedup_and_view() {
        let conn = memory();
        let mut sample = Sample { cpu_pct: Some(10.0), rx_bytes: Some(0), tx_bytes: Some(0), ..Default::default() };
        record(&conn, 1000, &sample, &[event("a", 1000, "aceito")]).unwrap();
        sample.cpu_pct = Some(30.0);
        sample.rx_bytes = Some(6000);
        sample.tx_bytes = Some(600);
        sample.memory = Some(Memory {
            total_bytes: 400, used_bytes: 100, used_pct: 25.0, free_bytes: 300,
            parts: vec![MemoryPart { label: "Usada".into(), bytes: 100, ring: true }],
        });
        let rates = record(&conn, 1060, &sample, &[event("a", 1000, "aceito"), event("b", 1001, "falha")]).unwrap();
        assert_eq!(rates, (Some(800.0), Some(80.0)));
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM ssh_events", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 2);
        let view = ssh_view(&conn, 1060).unwrap();
        assert_eq!(view.counts["24h"]["aceito"], 1);
        assert_eq!(view.counts["24h"]["falha"], 1);
        assert_eq!((view.origins_7d[0].accepted, view.origins_7d[0].refused), (1, 1));
        let points = series(&conn, 1060, 3600, 60).unwrap();
        assert_eq!(points.iter().map(|p| p.cpu_pct).collect::<Vec<_>>(), vec![Some(10.0), Some(30.0)]);
        assert_eq!(points[1].mem_p1_bytes, Some(100.0));
    }

    #[test]
    fn devices_keep_first_seen() {
        let conn = memory();
        let neighbor = Neighbor { ip: "10.0.0.5".into(), mac: "aa:bb".into(), state: "REACHABLE".into(), vendor: String::new(), label: String::new() };
        record_devices(&conn, 100, std::slice::from_ref(&neighbor)).unwrap();
        record_devices(&conn, 200, &[neighbor]).unwrap();
        let connected: HashSet<String> = ["aa:bb".to_string()].into();
        let view = devices_view(&conn, &connected).unwrap();
        assert_eq!((view[0].first_seen, view[0].last_seen, view[0].connected), (100, 200, true));
    }

    #[test]
    fn meta_roundtrip() {
        let conn = memory();
        assert_eq!(meta_get(&conn, "x"), None);
        meta_set(&conn, "x", "1").unwrap();
        meta_set(&conn, "x", "2").unwrap();
        assert_eq!(meta_get(&conn, "x").as_deref(), Some("2"));
    }
}

/// Quem entrou: um par usuário + chave + método, com as origens de onde veio.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Identity {
    pub user: String,
    pub key: String,
    pub method: String,
    pub entries: i64,
    pub last_ts: i64,
    pub origins: Vec<OriginCount>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OriginCount {
    pub ip: String,
    pub entries: i64,
    /// Primeira entrada aceita desta origem em todo o histórico guardado.
    pub first_ts: i64,
}

/// Tentativas sem login numa janela.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Attempts {
    pub total: i64,
    pub addresses: i64,
    pub top_addresses: Vec<(String, i64)>,
    pub top_users: Vec<(String, i64)>,
    /// Todas as origens da janela, para separar internet de rede local e Tailscale.
    pub per_address: Vec<(String, i64)>,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct SshFacts {
    pub identities: Vec<Identity>,
    pub attempts_24h: Attempts,
    pub since_ts: Option<i64>,
}

pub fn ssh_facts(conn: &Connection, now: i64) -> rusqlite::Result<SshFacts> {
    let week = now - 7 * 24 * 3600;
    let mut first_by_ip = std::collections::HashMap::new();
    let mut statement = conn.prepare_cached("SELECT ip, MIN(ts) FROM ssh_events WHERE kind = 'aceito' GROUP BY ip")?;
    for row in statement.query_map([], |row| Ok((row.get::<_, Option<String>>(0)?.unwrap_or_default(), row.get::<_, i64>(1)?)))? {
        let (ip, first) = row?;
        first_by_ip.insert(ip, first);
    }
    let mut statement = conn.prepare_cached(
        "SELECT COALESCE(user, ''), COALESCE(key, ''), COALESCE(method, ''), COALESCE(ip, ''), COUNT(*), MAX(ts)
         FROM ssh_events WHERE kind = 'aceito' AND ts >= ?1
         GROUP BY 1, 2, 3, 4 ORDER BY MAX(ts) DESC",
    )?;
    let mut identities: Vec<Identity> = Vec::new();
    for row in statement.query_map([week], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?, row.get::<_, i64>(5)?))
    })? {
        let (user, key, method, ip, entries, last_ts) = row?;
        let origin = OriginCount { first_ts: first_by_ip.get(&ip).copied().unwrap_or(last_ts), ip, entries };
        match identities.iter_mut().find(|item| item.user == user && item.key == key && item.method == method) {
            Some(item) => {
                item.entries += entries;
                item.last_ts = item.last_ts.max(last_ts);
                item.origins.push(origin);
            }
            None => identities.push(Identity { user, key, method, entries, last_ts, origins: vec![origin] }),
        }
    }
    for item in &mut identities {
        item.origins.sort_by(|a, b| b.entries.cmp(&a.entries).then_with(|| a.ip.cmp(&b.ip)));
    }

    let day = now - 24 * 3600;
    let (total, addresses) = conn.query_row(
        "SELECT COUNT(*), COUNT(DISTINCT ip) FROM ssh_events WHERE kind != 'aceito' AND ts >= ?1",
        [day],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    let top = |sql: &str| -> rusqlite::Result<Vec<(String, i64)>> {
        let mut statement = conn.prepare_cached(sql)?;
        statement.query_map([day], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?.collect()
    };
    let top_addresses = top(
        "SELECT ip, COUNT(*) FROM ssh_events WHERE kind != 'aceito' AND ts >= ?1 AND COALESCE(ip, '') != ''
         GROUP BY ip ORDER BY COUNT(*) DESC, ip LIMIT 3",
    )?;
    let top_users = top(
        "SELECT user, COUNT(*) FROM ssh_events WHERE kind != 'aceito' AND ts >= ?1 AND COALESCE(user, '') != ''
         GROUP BY user ORDER BY COUNT(*) DESC, user LIMIT 5",
    )?;
    let per_address = top(
        "SELECT ip, COUNT(*) FROM ssh_events WHERE kind != 'aceito' AND ts >= ?1 AND COALESCE(ip, '') != ''
         GROUP BY ip ORDER BY COUNT(*) DESC, ip",
    )?;
    let since_ts = conn.query_row("SELECT MIN(ts) FROM ssh_events", [], |row| row.get(0))?;
    Ok(SshFacts { identities, attempts_24h: Attempts { total, addresses, top_addresses, top_users, per_address }, since_ts })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SeenListener {
    pub port: u16,
    pub address: String,
    pub process: String,
    pub first_seen: i64,
}

/// Guarda quando cada porta apareceu. Devolve as abertas agora e o início do registro.
pub fn record_listeners(conn: &Connection, now: i64, listeners: &[crate::model::Listener]) -> rusqlite::Result<(Vec<SeenListener>, i64)> {
    for item in listeners {
        conn.execute(
            "INSERT INTO listeners_seen (port, address, process, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)
             ON CONFLICT(port, address) DO UPDATE SET process = excluded.process, last_seen = excluded.last_seen",
            params![item.port, item.address, item.process, now],
        )?;
    }
    conn.execute("DELETE FROM listeners_seen WHERE last_seen < ?1", [now - LISTENER_RETENTION])?;
    let baseline: i64 = conn.query_row("SELECT COALESCE(MIN(first_seen), ?1) FROM listeners_seen", [now], |row| row.get(0))?;
    let mut statement = conn.prepare_cached(
        "SELECT port, address, COALESCE(process, ''), first_seen FROM listeners_seen WHERE last_seen = ?1 ORDER BY port",
    )?;
    let open = statement
        .query_map([now], |row| Ok(SeenListener { port: row.get(0)?, address: row.get(1)?, process: row.get(2)?, first_seen: row.get(3)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((open, baseline))
}
