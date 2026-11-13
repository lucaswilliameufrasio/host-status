//! Checagens comuns: sshd e serviços declarados na configuração.

use crate::config::Service;
use crate::model::{Check, Listener, State};
use crate::util;
use std::collections::BTreeMap;

/// Primeiro valor de cada chave, como o sshd lê. Chaves em minúsculas: o OpenSSH 10
/// escreve `sshd -T` em CamelCase.
pub fn sshd_settings(text: &str) -> BTreeMap<String, String> {
    let mut settings = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(char::is_whitespace) {
            settings.entry(key.to_lowercase()).or_insert_with(|| value.trim().to_string());
        }
    }
    settings
}

pub fn sshd_checks(settings: &BTreeMap<String, String>) -> Vec<Check> {
    let get = |key: &str| settings.get(key).map(String::as_str);
    let mut checks = Vec::new();
    let password_off = get("passwordauthentication") == Some("no")
        && get("kbdinteractiveauthentication").unwrap_or("no") == "no";
    checks.push(if password_off {
        Check::new("ssh_senha", "SSH sem senha", State::Ok, "só chave pública")
    } else {
        Check::new("ssh_senha", "SSH sem senha", State::Problema, "o sshd aceita senha")
    });
    checks.push(match get("permitrootlogin") {
        Some("no") => Check::new("ssh_root", "Login de root por SSH", State::Ok, "bloqueado"),
        Some("prohibit-password") | Some("without-password") => {
            Check::new("ssh_root", "Login de root por SSH", State::Atencao, "só com chave")
        }
        _ => Check::new("ssh_root", "Login de root por SSH", State::Problema, "permitido"),
    });
    checks.push(match get("allowusers") {
        Some(users) => Check::new("ssh_usuarios", "Usuários aceitos no SSH", State::Ok, users),
        None => Check::new("ssh_usuarios", "Usuários aceitos no SSH", State::Atencao, "sem AllowUsers"),
    });
    checks
}

/// Uma porta que já aparece ouvindo não é testada por conexão: conectar no sshd, por exemplo,
/// vira uma "conexão sem login" a cada coleta no próprio registro que o painel mostra.
pub fn service_checks(services: &[Service], listeners: &[Listener]) -> Vec<Check> {
    let tailscale = util::tailscale_ipv4().map(|address| address.to_string());
    services
        .iter()
        .enumerate()
        .map(|(index, service)| {
            let host = if service.host == "tailscale" { tailscale.clone() } else { Some(service.host.clone()) };
            let id = format!("servico_{index}");
            let listening = |host: &str| {
                listeners.iter().any(|item| item.port == service.port && (item.address == "todas as interfaces" || item.address == host))
            };
            match host {
                Some(host) if listening(&host) => {
                    Check::new(&id, &service.label, State::Ok, format!("ouvindo na porta {}", service.port))
                }
                Some(host) if util::probe_tcp(&host, service.port) => {
                    Check::new(&id, &service.label, State::Ok, format!("respondendo na porta {}", service.port))
                }
                Some(_) => Check::new(&id, &service.label, State::Problema, format!("sem resposta na porta {}", service.port)),
                None => Check::new(&id, &service.label, State::Desconhecido, "sem IP da Tailscale"),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listening_port_is_ok_without_connecting() {
        let services = vec![Service { label: "SSH".into(), port: 22, host: "127.0.0.1".into() }];
        let listeners = vec![Listener { port: 22, address: "todas as interfaces".into(), process: "sshd".into() }];
        let checks = service_checks(&services, &listeners);
        assert_eq!((checks[0].state, checks[0].detail.as_str()), (State::Ok, "ouvindo na porta 22"));
    }

    #[test]
    fn first_value_wins_and_keys_are_lowercase() {
        let settings = sshd_settings("# x\nPasswordAuthentication no\nKbdInteractiveAuthentication no\nPermitRootLogin no\nAllowUsers admin\nPasswordAuthentication yes\n");
        assert_eq!(settings["passwordauthentication"], "no");
        let states: Vec<State> = sshd_checks(&settings).iter().map(|c| c.state).collect();
        assert_eq!(states, vec![State::Ok, State::Ok, State::Ok]);
    }

    #[test]
    fn defaults_are_flagged() {
        let states: Vec<State> = sshd_checks(&sshd_settings("PermitRootLogin prohibit-password\n")).iter().map(|c| c.state).collect();
        assert_eq!(states, vec![State::Problema, State::Atencao, State::Atencao]);
    }
}
