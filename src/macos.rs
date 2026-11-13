//! Leitura dos formatos do macOS. Funções puras, testadas em qualquer sistema.

use crate::checks;
use crate::model::{Check, Listener, Memory, MemoryPart, SshEvent, State};
use crate::ssh;
use std::collections::BTreeMap;

/// Dias desde 1970-01-01 para uma data do calendário gregoriano (algoritmo de Howard Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// "2026-11-11 14:40:02.512345-0300" → segundos desde 1970 (UTC).
pub fn log_timestamp(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once(' ')?;
    let mut parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    let sign_at = rest.rfind(['+', '-'])?;
    let (clock, zone) = rest.split_at(sign_at);
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.split('.').next()?.parse().ok()?;
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let zone = &zone[1..];
    if zone.len() != 4 {
        return None;
    }
    let offset = sign * (zone[..2].parse::<i64>().ok()? * 3600 + zone[2..].parse::<i64>().ok()? * 60);
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second - offset)
}

/// Saída de `log show --style ndjson`, já filtrada para o sshd.
pub fn log_events(text: &str) -> Vec<SshEvent> {
    text.lines()
        .filter_map(|line| {
            let entry: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
            let message = entry.get("eventMessage")?.as_str()?;
            let stamp = entry.get("timestamp")?.as_str()?;
            let parsed = ssh::parse(message)?;
            let pid = entry.get("processID").map(|v| v.to_string()).unwrap_or_default();
            Some(SshEvent {
                uid: format!("{stamp}|{pid}"),
                ts: log_timestamp(stamp)?,
                kind: parsed.kind.into(),
                method: parsed.method,
                user: parsed.user,
                ip: parsed.ip,
                key: parsed.key,
            })
        })
        .collect()
}

/// `route -n get default`: (interface, gateway).
pub fn route(text: &str) -> (Option<String>, Option<String>) {
    let mut interface = None;
    let mut gateway = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(':') {
            match key.trim() {
                "interface" => interface = Some(value.trim().to_string()),
                "gateway" => gateway = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    (interface, gateway)
}

/// Linha `<Link#N>` da interface em `netstat -ibn`: (Ibytes, Obytes) em 64 bits.
pub fn netstat_bytes(text: &str, interface: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 10 || fields[0] != interface || !fields[2].starts_with("<Link") {
            return None;
        }
        // Sem endereço MAC a linha tem uma coluna a menos.
        let shift = usize::from(!fields[3].contains(':'));
        Some((fields[6 - shift].parse().ok()?, fields[9 - shift].parse().ok()?))
    })
}

/// `lsof -nP -iTCP -sTCP:LISTEN`: portas fora do loopback, com o processo. Só enxerga
/// processos do usuário; os serviços do sistema entram por outro caminho.
pub fn lsof_listeners(text: &str) -> Vec<Listener> {
    let mut found = BTreeMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(name) = fields.iter().rev().find(|field| field.contains(':') && !field.starts_with('(')) else { continue };
        let Some((address, port)) = name.rsplit_once(':') else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        let address = address.trim_matches(|c| c == '[' || c == ']');
        if address.starts_with("127.") || address == "::1" || address.starts_with("fe80:") {
            continue;
        }
        let scope = if address == "*" { "todas as interfaces".to_string() } else { address.to_string() };
        found.entry((port, scope)).or_insert_with(|| fields[0].replace("\\x20", " "));
    }
    found.into_iter().map(|((port, address), process)| Listener { port, address, process }).collect()
}

pub fn launchctl_disabled(text: &str) -> BTreeMap<String, bool> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, state) = line.split_once("=>")?;
            Some((name.trim().trim_matches('"').to_string(), state.trim() == "disabled"))
        })
        .collect()
}

pub fn memory(total: u64, page: u64, active: u64, wired: u64, compressed: u64) -> Option<Memory> {
    if total == 0 {
        return None;
    }
    let (active, wired, compressed) = (active * page, wired * page, compressed * page);
    // Mesma conta do Monitor de Atividade: ativa + wired + comprimida.
    let used = (active + wired + compressed).min(total);
    Some(Memory {
        total_bytes: total,
        used_bytes: used,
        used_pct: crate::store::round(used as f64 / total as f64 * 100.0, 1),
        free_bytes: total - used,
        parts: vec![
            MemoryPart { label: "Ativa".into(), bytes: active, ring: true },
            MemoryPart { label: "Wired".into(), bytes: wired, ring: true },
            MemoryPart { label: "Comprimida".into(), bytes: compressed, ring: true },
        ],
    })
}

/// O que o macOS diz sobre acesso remoto e proteção. `None` quando não deu para ler.
#[derive(Debug, Default)]
pub struct Facts {
    pub hardening: Option<String>,
    pub authorized_keys: Option<usize>,
    pub launchd: Option<BTreeMap<String, bool>>,
    pub firewall: Option<String>,
    pub filevault: Option<String>,
    pub sip: Option<String>,
    pub remote_tools: Vec<(String, bool)>,
}

pub fn posture(facts: &Facts) -> Vec<Check> {
    let mut list = Vec::new();
    match &facts.hardening {
        Some(text) => list.extend(checks::sshd_checks(&checks::sshd_settings(text))),
        None => list.push(Check::new("ssh_senha", "SSH sem senha", State::Problema, "000-hardening.conf não está instalado; o sshd aceita senha")),
    }
    list.push(match facts.authorized_keys {
        None => Check::new("ssh_chaves", "Chaves autorizadas", State::Desconhecido, "não consegui ler"),
        Some(0) => Check::new("ssh_chaves", "Chaves autorizadas", State::Problema, "nenhuma chave"),
        Some(count) => Check::new("ssh_chaves", "Chaves autorizadas", State::Ok, count.to_string()),
    });
    for (id, service, label) in [
        ("tela", "com.apple.screensharing", "Compartilhamento de tela"),
        ("apple_events", "com.apple.AEServer", "Remote Apple Events"),
    ] {
        list.push(match facts.launchd.as_ref().map(|map| map.get(service).copied().unwrap_or(false)) {
            None => Check::new(id, label, State::Desconhecido, "launchctl não respondeu"),
            Some(true) => Check::new(id, label, State::Ok, "desligado"),
            Some(false) => Check::new(id, label, State::Atencao, "não está desabilitado"),
        });
    }
    list.push(match facts.firewall.as_deref() {
        None => Check::new("firewall", "Firewall do macOS", State::Desconhecido, "sem resposta"),
        Some(text) if text.contains("Firewall is enabled") && text.contains("stealth mode is on") => {
            Check::new("firewall", "Firewall do macOS", State::Ok, "ligado, modo furtivo")
        }
        Some(text) if text.contains("Firewall is enabled") => Check::new("firewall", "Firewall do macOS", State::Atencao, "ligado, sem modo furtivo"),
        Some(_) => Check::new("firewall", "Firewall do macOS", State::Problema, "desligado"),
    });
    for (id, label, text, on) in [
        ("filevault", "FileVault", &facts.filevault, "FileVault is On"),
        ("sip", "Proteção de integridade (SIP)", &facts.sip, "enabled"),
    ] {
        list.push(match text.as_deref() {
            None => Check::new(id, label, State::Desconhecido, "sem resposta"),
            Some(text) if text.contains(on) => Check::new(id, label, State::Ok, "ligado"),
            Some(_) => Check::new(id, label, State::Problema, "desligado"),
        });
    }
    for (name, running) in &facts.remote_tools {
        let detail = if *running { "instalado e em execução" } else { "instalado, parado; remova se não usa" };
        list.push(Check::new(&format!("app_{}", name.to_lowercase()), name, State::Atencao, detail));
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(log_timestamp("2026-11-11 14:40:02.512345-0300"), Some(1794418802));
        assert_eq!(log_timestamp("1970-01-01 00:00:00.000000+0000"), Some(0));
        assert_eq!(log_timestamp("2024-02-29 12:00:00.0+0100"), Some(1709204400));
        assert_eq!(log_timestamp("lixo"), None);
    }

    #[test]
    fn unified_log() {
        let text = [
            r#"{"timestamp":"2026-11-11 14:40:02.512345-0300","processID":7,"eventMessage":"Accepted publickey for admin from 10.0.0.1 port 5 ssh2: ED25519 SHA256:k"}"#,
            r#"{"timestamp":"2026-11-11 14:40:03.000000-0300","eventMessage":"in pam_sm_setcred(): nothing"}"#,
            r#"{"count":2,"finished":1}"#,
        ]
        .join("\n");
        let events = log_events(&text);
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].ts, events[0].uid.as_str()), (1794418802, "2026-11-11 14:40:02.512345-0300|7"));
    }

    #[test]
    fn default_route() {
        assert_eq!(route("   route to: default\n  gateway: 192.168.1.1\n  interface: en0\n"), (Some("en0".into()), Some("192.168.1.1".into())));

    }

    #[test]
    fn lsof_ports() {
        let text = "COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n\
                    ControlCe 1 u 9u IPv4 0x1 0t0 TCP *:5000 (LISTEN)\n\
                    ControlCe 1 u 10u IPv6 0x2 0t0 TCP *:5000 (LISTEN)\n\
                    host-stat 2 u 7u IPv4 0x3 0t0 TCP 127.0.0.1:8787 (LISTEN)\n\
                    Code\\x20H 3 u 8u IPv4 0x4 0t0 TCP 192.168.1.103:3000 (LISTEN)\n";
        let found = lsof_listeners(text);
        assert_eq!(found, vec![
            Listener { port: 3000, address: "192.168.1.103".into(), process: "Code H".into() },
            Listener { port: 5000, address: "todas as interfaces".into(), process: "ControlCe".into() },
        ]);
    }

    #[test]
    fn interface_counters() {
        let text = "Name  Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll\n\
                    en0   1500  <Link#8>    00:00:5e:00:53:03 1165304936     0 1272874761629 101093451     0 21129184189     0\n\
                    en0   1500  192.168.1/24   192.168.1.103    1165304936     - 1272874761629 101093451     - 21129184189     -\n\
                    utun0 1380  <Link#12>                     10     0 2000 20     0 3000     0\n";
        assert_eq!(netstat_bytes(text, "en0"), Some((1272874761629, 21129184189)));
        assert_eq!(netstat_bytes(text, "utun0"), Some((2000, 3000)));
        assert_eq!(netstat_bytes(text, "en9"), None);
    }

    #[test]
    fn memory_and_posture() {
        let memory = memory(400 * 16384, 16384, 100, 50, 50).unwrap();
        assert_eq!(memory.used_pct, 50.0);
        let disabled = launchctl_disabled("\t\t\"com.apple.screensharing\" => disabled\n\t\t\"com.openssh.sshd\" => enabled\n");
        let facts = Facts {
            hardening: Some("PasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin no\nAllowUsers admin\n".into()),
            authorized_keys: Some(1),
            launchd: Some(disabled),
            firewall: Some("Firewall is enabled. (State = 1)\nFirewall stealth mode is on\n".into()),
            filevault: Some("FileVault is On.".into()),
            sip: Some("System Integrity Protection status: enabled.".into()),
            remote_tools: vec![("AnyDesk".into(), false)],
        };
        let states: BTreeMap<String, State> = posture(&facts).into_iter().map(|c| (c.id, c.state)).collect();
        assert_eq!(states["ssh_senha"], State::Ok);
        assert_eq!(states["tela"], State::Ok);
        assert_eq!(states["apple_events"], State::Atencao);
        assert_eq!(states["firewall"], State::Ok);
        assert_eq!(states["app_anydesk"], State::Atencao);
        assert_eq!(posture(&Facts::default())[0].state, State::Problema);
    }
}
