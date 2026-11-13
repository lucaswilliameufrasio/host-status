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
        CREATE TABLE IF NOT EXISTS devices (
            mac TEXT PRIMARY KEY, ip TEXT, vendor TEXT, label TEXT,
            first_seen INTEGER, last_seen INTEGER, last_state TEXT
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
