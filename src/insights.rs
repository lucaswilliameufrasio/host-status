//! Leitura: conclusões tiradas do que o coletor já guarda, para não ficar só na tabela crua.
//! Cada leitura é uma `Check` (estado, item, detalhe), como as checagens de proteção.

use crate::model::{Check, Fail2ban, State};
use crate::store::{Device, SeenListener, SshFacts};
use serde::Serialize;
use std::collections::HashMap;
use std::net::IpAddr;

const DAY: i64 = 24 * 3600;
/// Portas a partir daqui são efêmeras (Docker, VS Code, depuradores): mudam o tempo todo.
const EPHEMERAL_PORTS: u16 = 32768;

pub struct Facts<'a> {
    pub now: i64,
    pub ssh: &'a SshFacts,
    /// Impressão digital (`SHA256:…`) → comentário da chave em algum `authorized_keys`.
    pub key_labels: &'a HashMap<String, String>,
    /// `Some(true)` quando o sshd aceita senha; `None` quando não deu para ler.
    pub password_login: Option<bool>,
    pub fail2ban: Option<&'a Fail2ban>,
    /// `None` onde não se aplica (macOS).
    pub failed_units: Option<&'a [String]>,
    pub oom_kills: Option<&'a [(String, i64)]>,
    pub listeners: &'a [SeenListener],
    pub listeners_since: i64,
    pub devices: Option<&'a [Device]>,
}

/// Uma linha de "Quem entrou".
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Who {
    pub user: String,
    pub device: String,
    pub key: String,
    pub method: String,
    pub entries: i64,
    pub last_ts: i64,
    pub origins: Vec<WhoOrigin>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WhoOrigin {
    pub ip: String,
    pub entries: i64,
    pub network: &'static str,
}

/// De onde vem um endereço: muda o peso de uma entrada ou de uma porta aberta.
pub fn network(address: &str) -> &'static str {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            if ip.is_loopback() {
                "local"
            } else if a == 100 && (64..=127).contains(&b) {
                "tailscale"
            } else if ip.is_private() || ip.is_link_local() {
                "rede local"
            } else {
                "internet"
            }
        }
        Ok(IpAddr::V6(ip)) => {
            let first = ip.segments()[0];
            if ip.is_loopback() {
                "local"
            } else if ip.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0] {
                "tailscale"
            } else if first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80 {
                "rede local"
            } else {
                "internet"
            }
        }
        Err(_) => "desconhecida",
    }
}

pub fn who(facts: &Facts) -> Vec<Who> {
    facts
        .ssh
        .identities
        .iter()
        .map(|item| Who {
            user: item.user.clone(),
            device: facts.key_labels.get(&item.key).cloned().unwrap_or_default(),
            key: item.key.clone(),
            method: item.method.clone(),
            entries: item.entries,
            last_ts: item.last_ts,
            origins: item
                .origins
                .iter()
                .map(|origin| WhoOrigin { ip: origin.ip.clone(), entries: origin.entries, network: network(&origin.ip) })
                .collect(),
        })
        .collect()
}

pub fn readings(facts: &Facts) -> Vec<Check> {
    let mut list = vec![ssh_logins(facts), ssh_attempts(facts)];
    if let Some(units) = facts.failed_units {
        list.push(failed_units(units));
    }
    if let Some(kills) = facts.oom_kills {
        list.push(oom_kills(kills));
    }
    list.push(new_ports(facts));
    if let Some(devices) = facts.devices {
        list.push(new_devices(devices, facts.now));
    }
    list
}

fn ssh_logins(facts: &Facts) -> Check {
    const ID: &str = "leitura_ssh_entradas";
    const LABEL: &str = "Quem entrou por SSH (7 dias)";
    let identities = &facts.ssh.identities;
    if identities.is_empty() {
        return Check::new(ID, LABEL, State::Ok, "ninguém entrou por SSH nos últimos 7 dias");
    }
    let total: i64 = identities.iter().map(|item| item.entries).sum();
    let mut origins: Vec<(&str, i64)> = Vec::new();
    for origin in identities.iter().flat_map(|item| &item.origins) {
        match origins.iter_mut().find(|(ip, _)| *ip == origin.ip) {
            Some((_, entries)) => *entries += origin.entries,
            None => origins.push((&origin.ip, origin.entries)),
        }
    }
    origins.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let origin_text = list_with_more(origins.iter().map(|(ip, entries)| format!("{ip} ({}, {})", network(ip), count(*entries))), 3);
    let mut devices: Vec<String> = identities
        .iter()
        .filter(|item| !item.key.is_empty())
        .map(|item| facts.key_labels.get(&item.key).cloned().unwrap_or_else(|| item.key.clone()))
        .collect();
    devices.sort();
    devices.dedup();
    let by_key = identities.iter().all(|item| item.method == "publickey");
    let summary = format!(
        "{} {} em 7 dias, {}, de {} {}: {}{}",
        count(total),
        plural(total, "entrada", "entradas"),
        if by_key { "todas por chave" } else { "nem todas por chave" },
        origins.len(),
        plural(origins.len() as i64, "origem", "origens"),
        origin_text,
        if devices.is_empty() { String::new() } else { format!(". Chaves: {}", devices.join(", ")) },
    );

    let not_by_key: Vec<String> = identities
        .iter()
        .filter(|item| item.method != "publickey")
        .map(|item| format!("{} por {} ({})", item.user, method_name(&item.method), count(item.entries)))
        .collect();
    if !not_by_key.is_empty() {
        return Check::new(ID, LABEL, State::Problema, format!("entrada sem chave: {}. {summary}", not_by_key.join(", ")));
    }
    let new_public: Vec<String> = identities
        .iter()
        .flat_map(|item| item.origins.iter().map(move |origin| (item, origin)))
        .filter(|(_, origin)| {
            network(&origin.ip) == "internet"
                && origin.first_ts >= facts.now - DAY
                && facts.ssh.since_ts.is_some_and(|since| origin.first_ts - since >= DAY)
        })
        .map(|(item, origin)| format!("{} como {}", origin.ip, item.user))
        .collect();
    if !new_public.is_empty() {
        return Check::new(ID, LABEL, State::Atencao, format!("origem nova pela internet nas últimas 24 h: {}. {summary}", new_public.join(", ")));
    }
    let root: i64 = identities.iter().filter(|item| item.user == "root").map(|item| item.entries).sum();
    if root > 0 {
        return Check::new(ID, LABEL, State::Atencao, format!("o root entrou direto {} {}. {summary}", count(root), plural(root, "vez", "vezes")));
    }
    Check::new(ID, LABEL, State::Ok, summary)
}

fn ssh_attempts(facts: &Facts) -> Check {
    const ID: &str = "leitura_ssh_tentativas";
    const LABEL: &str = "Tentativas sem login (24 h)";
    let attempts = &facts.ssh.attempts_24h;
    if attempts.total == 0 {
        return Check::new(ID, LABEL, State::Ok, "nenhuma tentativa sem login nas últimas 24 horas");
    }
    let mut detail = format!(
        "{} {} de {} {}",
        count(attempts.total),
        plural(attempts.total, "tentativa", "tentativas"),
        count(attempts.addresses),
        plural(attempts.addresses, "endereço", "endereços"),
    );
    if !attempts.top_addresses.is_empty() {
        let top: Vec<String> = attempts.top_addresses.iter().map(|(ip, total)| format!("{ip} ({}, {})", network(ip), count(*total))).collect();
        detail.push_str(&format!("; mais insistentes: {}", top.join(", ")));
    }
    if !attempts.top_users.is_empty() {
        let users: Vec<&str> = attempts.top_users.iter().map(|(user, _)| user.as_str()).collect();
        detail.push_str(&format!("; usuários mais tentados: {}", users.join(", ")));
    }
    if let Some(fail2ban) = facts.fail2ban {
        detail.push_str(&format!(
            "; fail2ban: {} {} agora, {} no total",
            count(fail2ban.currently_banned as i64),
            plural(fail2ban.currently_banned as i64, "banido", "banidos"),
            count(fail2ban.total_banned as i64),
        ));
    }
    match facts.password_login {
        Some(true) => Check::new(ID, LABEL, State::Problema, format!("o SSH aceita senha e está recebendo tentativas: {detail}")),
        Some(false) if facts.fail2ban.is_none() && attempts.total >= 100 => {
            Check::new(ID, LABEL, State::Atencao, format!("só chave é aceita, mas sem fail2ban para barrar a insistência: {detail}"))
        }
        Some(false) => Check::new(ID, LABEL, State::Ok, format!("nenhuma tem como entrar, porque o SSH só aceita chave: {detail}")),
        None => Check::new(ID, LABEL, State::Atencao, detail),
    }
}

fn failed_units(units: &[String]) -> Check {
    const ID: &str = "leitura_servicos";
    const LABEL: &str = "Serviços com falha";
    if units.is_empty() {
        return Check::new(ID, LABEL, State::Ok, "nenhum serviço do systemd em falha");
    }
    Check::new(ID, LABEL, State::Problema, format!("{} em falha: {}", units.len(), list_with_more(units.iter().cloned(), 5)))
}

fn oom_kills(kills: &[(String, i64)]) -> Check {
    const ID: &str = "leitura_memoria";
    const LABEL: &str = "Falta de memória (24 h)";
    if kills.is_empty() {
        return Check::new(ID, LABEL, State::Ok, "nenhum processo encerrado por falta de memória");
    }
    let total: i64 = kills.iter().map(|(_, total)| total).sum();
    let names = list_with_more(kills.iter().map(|(name, total)| format!("{name} ({})", count(*total))), 5);
    Check::new(ID, LABEL, State::Problema, format!("{} {} pelo kernel: {names}", count(total), plural(total, "processo encerrado", "processos encerrados")))
}

fn new_ports(facts: &Facts) -> Check {
    const ID: &str = "leitura_portas";
    const LABEL: &str = "Portas novas abertas (24 h)";
    let exposed = |item: &&SeenListener| item.address == "todas as interfaces" || network(&item.address) == "internet";
    let fresh: Vec<String> = facts
        .listeners
        .iter()
        .filter(exposed)
        .filter(|item| item.port < EPHEMERAL_PORTS && item.first_seen > facts.listeners_since && item.first_seen >= facts.now - DAY)
        .map(|item| if item.process.is_empty() { item.port.to_string() } else { format!("{} ({})", item.port, item.process) })
        .collect();
    if !fresh.is_empty() {
        return Check::new(ID, LABEL, State::Atencao, format!("passaram a aceitar conexão de qualquer rede: {}", list_with_more(fresh.into_iter(), 5)));
    }
    let open = facts.listeners.iter().filter(exposed).count() as i64;
    Check::new(ID, LABEL, State::Ok, format!("nenhuma porta nova; {} {} para qualquer rede", count(open), plural(open, "aberta", "abertas")))
}

fn new_devices(devices: &[Device], now: i64) -> Check {
    const ID: &str = "leitura_aparelhos";
    const LABEL: &str = "Aparelhos novos na rede (24 h)";
    let Some(since) = devices.iter().map(|item| item.first_seen).min() else {
        return Check::new(ID, LABEL, State::Ok, "nenhum aparelho registrado ainda");
    };
    let fresh: Vec<String> = devices
        .iter()
        .filter(|item| item.first_seen > since && item.first_seen >= now - DAY)
        .map(|item| {
            let name = [&item.label, &item.vendor].into_iter().find(|text| !text.is_empty()).cloned().unwrap_or_else(|| "sem nome".into());
            format!("{name} em {} ({})", item.ip, item.mac)
        })
        .collect();
    if fresh.is_empty() {
        return Check::new(ID, LABEL, State::Ok, format!("nenhum aparelho novo; {} {} conhecidos", count(devices.len() as i64), plural(devices.len() as i64, "aparelho", "aparelhos")));
    }
    Check::new(ID, LABEL, State::Atencao, format!("{} {}: {}", fresh.len(), plural(fresh.len() as i64, "apareceu", "apareceram"), list_with_more(fresh.into_iter(), 5)))
}

fn method_name(method: &str) -> &str {
    match method {
        "password" => "senha",
        "keyboard-interactive" | "keyboard-interactive/pam" => "senha (teclado)",
        "" => "método desconhecido",
        other => other,
    }
}

fn plural<'a>(total: i64, one: &'a str, many: &'a str) -> &'a str {
    if total == 1 { one } else { many }
}

/// Número no formato brasileiro: 1.357.
fn count(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('.');
        }
        out.push(digit);
    }
    if value < 0 { format!("-{out}") } else { out }
}

fn list_with_more(items: impl Iterator<Item = String>, shown: usize) -> String {
    let items: Vec<String> = items.collect();
    if items.len() <= shown {
        return items.join(", ");
    }
    format!("{} e mais {}", items[..shown].join(", "), items.len() - shown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Attempts, Identity, OriginCount};

    const NOW: i64 = 100 * DAY;

    fn identity(user: &str, method: &str, key: &str, origins: &[(&str, i64, i64)]) -> Identity {
        Identity {
            user: user.into(),
            key: key.into(),
            method: method.into(),
            entries: origins.iter().map(|(_, entries, _)| entries).sum(),
            last_ts: NOW - 60,
            origins: origins.iter().map(|(ip, entries, first)| OriginCount { ip: (*ip).into(), entries: *entries, first_ts: *first }).collect(),
        }
    }

    fn facts<'a>(ssh: &'a SshFacts, labels: &'a HashMap<String, String>, password: Option<bool>) -> Facts<'a> {
        Facts {
            now: NOW,
            ssh,
            key_labels: labels,
            password_login: password,
            fail2ban: None,
            failed_units: None,
            oom_kills: None,
            listeners: &[],
            listeners_since: 0,
            devices: None,
        }
    }

    #[test]
    fn networks() {
        assert_eq!(network("100.84.241.108"), "tailscale");
        assert_eq!(network("192.168.1.10"), "rede local");
        assert_eq!(network("191.6.105.226"), "internet");
        assert_eq!(network("fd7a:115c:a1e0::1"), "tailscale");
        assert_eq!(network("127.0.0.1"), "local");
        assert_eq!(network("x"), "desconhecida");
    }

    #[test]
    fn logins_all_by_key_name_the_origins_and_devices() {
        let ssh = SshFacts {
            identities: vec![identity("karazhan", "publickey", "SHA256:k1", &[("191.6.105.226", 31, NOW - 5 * DAY)])],
            since_ts: Some(NOW - 7 * DAY),
            ..Default::default()
        };
        let labels = HashMap::from([("SHA256:k1".to_string(), "lucaseufrasio@ursoc".to_string())]);
        let check = ssh_logins(&facts(&ssh, &labels, Some(false)));
        assert_eq!(check.state, State::Ok);
        assert_eq!(check.detail, "31 entradas em 7 dias, todas por chave, de 1 origem: 191.6.105.226 (internet, 31). Chaves: lucaseufrasio@ursoc");
    }

    #[test]
    fn password_login_is_a_problem_and_root_or_new_origin_need_attention() {
        let labels = HashMap::new();
        let ssh = SshFacts {
            identities: vec![identity("deploy", "password", "", &[("192.0.2.9", 2, NOW - 3 * DAY)])],
            since_ts: Some(NOW - 7 * DAY),
            ..Default::default()
        };
        let check = ssh_logins(&facts(&ssh, &labels, Some(true)));
        assert_eq!(check.state, State::Problema);
        assert!(check.detail.starts_with("entrada sem chave: deploy por senha (2)"), "{}", check.detail);

        let ssh = SshFacts {
            identities: vec![identity("admin", "publickey", "SHA256:k", &[("203.0.113.5", 1, NOW - 3600)])],
            since_ts: Some(NOW - 7 * DAY),
            ..Default::default()
        };
        let check = ssh_logins(&facts(&ssh, &labels, Some(false)));
        assert_eq!(check.state, State::Atencao);
        assert!(check.detail.starts_with("origem nova pela internet nas últimas 24 h: 203.0.113.5 como admin"));

        // Na primeira coleta tudo é "novo": sem um dia de histórico antes, não acusa.
        let ssh = SshFacts { since_ts: Some(NOW - 7200), ..ssh };
        assert_eq!(ssh_logins(&facts(&ssh, &labels, Some(false))).state, State::Ok);

        let ssh = SshFacts {
            identities: vec![identity("root", "publickey", "SHA256:k", &[("100.64.0.2", 3, NOW - 5 * DAY)])],
            since_ts: Some(NOW - 7 * DAY),
            ..Default::default()
        };
        let check = ssh_logins(&facts(&ssh, &labels, Some(false)));
        assert_eq!(check.state, State::Atencao);
        assert!(check.detail.starts_with("o root entrou direto 3 vezes"));
    }

    #[test]
    fn attempts_weigh_against_password_and_fail2ban() {
        let labels = HashMap::new();
        let ssh = SshFacts {
            attempts_24h: Attempts {
                total: 2739,
                addresses: 53,
                top_addresses: vec![("203.0.113.7".into(), 900)],
                top_users: vec![("root".into(), 400), ("admin".into(), 200)],
            },
            ..Default::default()
        };
        let check = ssh_attempts(&facts(&ssh, &labels, Some(false)));
        assert_eq!(check.state, State::Atencao);
        assert!(check.detail.contains("2.739 tentativas de 53 endereços; mais insistentes: 203.0.113.7 (internet, 900); usuários mais tentados: root, admin"));

        let banned = Fail2ban { jail: "sshd".into(), currently_banned: 4, total_banned: 12, currently_failed: 0 };
        let mut with_fail2ban = facts(&ssh, &labels, Some(false));
        with_fail2ban.fail2ban = Some(&banned);
        let check = ssh_attempts(&with_fail2ban);
        assert_eq!(check.state, State::Ok);
        assert!(check.detail.starts_with("nenhuma tem como entrar"));
        assert!(check.detail.ends_with("fail2ban: 4 banidos agora, 12 no total"));

        assert_eq!(ssh_attempts(&facts(&ssh, &labels, Some(true))).state, State::Problema);
        assert_eq!(ssh_attempts(&facts(&SshFacts::default(), &labels, Some(true))).state, State::Ok);
    }

    #[test]
    fn units_memory_ports_and_devices() {
        assert_eq!(failed_units(&[]).state, State::Ok);
        let check = failed_units(&["backup.service".into()]);
        assert_eq!((check.state, check.detail.as_str()), (State::Problema, "1 em falha: backup.service"));

        let check = oom_kills(&[("host-status".into(), 81)]);
        assert_eq!((check.state, check.detail.as_str()), (State::Problema, "81 processos encerrados pelo kernel: host-status (81)"));

        let labels = HashMap::new();
        let ssh = SshFacts::default();
        let listeners = vec![
            SeenListener { port: 22, address: "todas as interfaces".into(), process: "sshd".into(), first_seen: NOW - 9 * DAY },
            SeenListener { port: 5432, address: "todas as interfaces".into(), process: "postgres".into(), first_seen: NOW - 600 },
            SeenListener { port: 8080, address: "100.64.0.3".into(), process: "app".into(), first_seen: NOW - 600 },
            SeenListener { port: 40123, address: "todas as interfaces".into(), process: "docker-proxy".into(), first_seen: NOW - 600 },
        ];
        let mut with_ports = facts(&ssh, &labels, Some(false));
        with_ports.listeners = &listeners;
        with_ports.listeners_since = NOW - 9 * DAY;
        let check = new_ports(&with_ports);
        assert_eq!((check.state, check.detail.as_str()), (State::Atencao, "passaram a aceitar conexão de qualquer rede: 5432 (postgres)"));
        with_ports.listeners_since = NOW - 600;
        assert_eq!(new_ports(&with_ports).state, State::Ok);

        let device = |mac: &str, first_seen: i64| Device {
            mac: mac.into(), ip: "192.168.1.50".into(), vendor: "Espressif".into(), label: String::new(),
            first_seen, last_seen: NOW, connected: true,
        };
        let check = new_devices(&[device("aa", NOW - 30 * DAY), device("bb", NOW - 3600)], NOW);
        assert_eq!((check.state, check.detail.as_str()), (State::Atencao, "1 apareceu: Espressif em 192.168.1.50 (bb)"));
        assert_eq!(new_devices(&[device("aa", NOW - 30 * DAY)], NOW).state, State::Ok);
    }

    #[test]
    fn brazilian_numbers() {
        assert_eq!((count(7), count(1357), count(1234567)), ("7".into(), "1.357".into(), "1.234.567".into()));
    }
}
