//! Mensagens de autenticação do OpenSSH. São iguais no macOS e no Linux.

/// Evento sem `uid` e `ts`, que dependem de onde a linha veio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    pub kind: &'static str,
    pub method: String,
    pub user: String,
    pub ip: String,
    pub key: String,
}

fn is_address(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
}

/// "<ip> port <n>" no começo de `rest`. Devolve o IP.
fn address_then_port(rest: &str) -> Option<&str> {
    let mut parts = rest.split(' ');
    let ip = parts.next()?;
    let word = parts.next()?;
    let port = parts.next()?;
    if is_address(ip) && word == "port" && port.chars().all(|c| c.is_ascii_digit()) && !port.is_empty() {
        Some(ip)
    } else {
        None
    }
}

pub fn parse(message: &str) -> Option<Parsed> {
    if let Some(rest) = message.strip_prefix("Accepted ") {
        let (method, rest) = rest.split_once(" for ")?;
        let (user, rest) = rest.split_once(" from ")?;
        let ip = address_then_port(rest)?;
        // "... port 22 ssh2: ED25519 SHA256:abc"
        let key = rest
            .split_once(" ssh2: ")
            .and_then(|(_, tail)| tail.split(' ').nth(1))
            .unwrap_or("")
            .to_string();
        return Some(Parsed { kind: "aceito", method: method.into(), user: user.into(), ip: ip.into(), key });
    }
    if let Some(rest) = message.strip_prefix("Failed ") {
        let (method, rest) = rest.split_once(" for ")?;
        let (kind, rest) = match rest.strip_prefix("invalid user ") {
            Some(tail) => ("usuario_invalido", tail),
            None => ("falha", rest),
        };
        let (user, rest) = rest.rsplit_once(" from ")?;
        let ip = address_then_port(rest)?;
        return Some(Parsed { kind, method: method.into(), user: user.into(), ip: ip.into(), key: String::new() });
    }
    if let Some(rest) = message.strip_prefix("Invalid user ") {
        let (user, rest) = rest.rsplit_once("from ")?;
        let ip = address_then_port(rest)?;
        return Some(Parsed {
            kind: "usuario_invalido",
            method: String::new(),
            user: user.trim_end().into(),
            ip: ip.into(),
            key: String::new(),
        });
    }
    if let Some(rest) = message.strip_prefix("Connection closed by ") {
        let rest = rest.strip_suffix(" [preauth]")?;
        let (user, rest) = if let Some(tail) = rest
            .strip_prefix("authenticating user ")
            .or_else(|| rest.strip_prefix("invalid user "))
        {
            let (user, address) = tail.split_once(' ')?;
            (user.to_string(), address)
        } else {
            (String::new(), rest)
        };
        let ip = address_then_port(rest)?;
        return Some(Parsed {
            kind: "fechou_antes_de_autenticar",
            method: String::new(),
            user,
            ip: ip.into(),
            key: String::new(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_with_key() {
        let event = parse("Accepted publickey for admin from 192.168.1.134 port 50080 ssh2: ED25519 SHA256:abc").unwrap();
        assert_eq!((event.kind, event.user.as_str(), event.ip.as_str(), event.key.as_str()),
                   ("aceito", "admin", "192.168.1.134", "SHA256:abc"));
    }

    #[test]
    fn accepted_without_key() {
        let event = parse("Accepted password for admin from fe80::1 port 22 ssh2").unwrap();
        assert_eq!((event.method.as_str(), event.key.as_str()), ("password", ""));
    }

    #[test]
    fn failures() {
        let failed = parse("Failed password for admin from 1.2.3.4 port 22 ssh2").unwrap();
        assert_eq!((failed.kind, failed.user.as_str()), ("falha", "admin"));
        let invalid = parse("Failed password for invalid user admin from 1.2.3.4 port 22 ssh2").unwrap();
        assert_eq!((invalid.kind, invalid.user.as_str()), ("usuario_invalido", "admin"));
        let bare = parse("Invalid user pi from fe80::1 port 9").unwrap();
        assert_eq!((bare.user.as_str(), bare.ip.as_str()), ("pi", "fe80::1"));
        let empty = parse("Invalid user  from 5.6.7.8 port 9").unwrap();
        assert_eq!(empty.user, "");
    }

    #[test]
    fn preauth_close() {
        let plain = parse("Connection closed by 192.168.1.114 port 40054 [preauth]").unwrap();
        assert_eq!((plain.kind, plain.ip.as_str(), plain.user.as_str()),
                   ("fechou_antes_de_autenticar", "192.168.1.114", ""));
        let named = parse("Connection closed by authenticating user admin 10.0.0.2 port 1 [preauth]").unwrap();
        assert_eq!((named.user.as_str(), named.ip.as_str()), ("admin", "10.0.0.2"));
        assert!(parse("Connection closed by 10.0.0.2 port 1").is_none());
    }

    #[test]
    fn ignores_other_lines() {
        assert!(parse("in pam_sm_setcred(): nothing").is_none());
        assert!(parse("Accepted publickey for admin from not-an-ip port 22").is_none());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Session {
    pub user: String,
    pub tty: String,
    pub origin: String,
}

/// Saída do `who`: sessões abertas. A origem vem entre parênteses no fim da linha.
pub fn parse_who(text: &str) -> Vec<Session> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 2 {
                return None;
            }
            let last = fields[fields.len() - 1];
            let origin = last.strip_prefix('(').and_then(|v| v.strip_suffix(')')).unwrap_or("").to_string();
            Some(Session { user: fields[0].into(), tty: fields[1].into(), origin })
        })
        .collect()
}

#[cfg(test)]
mod who_tests {
    use super::*;

    #[test]
    fn sessions() {
        let rows = parse_who("admin console      Oct 26 23:48 \nadmin pts/0  2026-11-11 13:46 (192.168.1.134)\n");
        assert_eq!(rows[1], Session { user: "admin".into(), tty: "pts/0".into(), origin: "192.168.1.134".into() });
        assert_eq!(rows[0].origin, "");
    }
}
