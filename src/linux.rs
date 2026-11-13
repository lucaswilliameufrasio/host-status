//! Leitura dos formatos do Linux. Funções puras, testadas em qualquer sistema.

use crate::model::{Fail2ban, Listener, Memory, MemoryPart, Process, Swap, SshEvent};
use crate::ssh;
use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Contadores da linha `cpu`: user nice system idle iowait irq softirq steal.
pub fn proc_stat_cpu(text: &str) -> Option<[u64; 8]> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let mut values = [0u64; 8];
    for (slot, part) in values.iter_mut().zip(line.split_whitespace().skip(1)) {
        *slot = part.parse().ok()?;
    }
    Some(values)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CpuParts {
    pub user: f64,
    pub system: f64,
    pub iowait: f64,
    pub idle: f64,
}

pub fn cpu_parts(previous: &[u64; 8], current: &[u64; 8]) -> Option<CpuParts> {
    let deltas: Vec<f64> = previous.iter().zip(current).map(|(a, b)| b.saturating_sub(*a) as f64).collect();
    let total: f64 = deltas.iter().sum();
    if total <= 0.0 {
        return None;
    }
    let pct = |value: f64| crate::store::round(value / total * 100.0, 1);
    Some(CpuParts {
        user: pct(deltas[0] + deltas[1]),
        system: pct(deltas[2] + deltas[5] + deltas[6] + deltas[7]),
        iowait: pct(deltas[4]),
        idle: pct(deltas[3]),
    })
}

pub fn boot_time(proc_stat: &str) -> Option<i64> {
    proc_stat.lines().find_map(|line| line.strip_prefix("btime ")?.trim().parse().ok())
}

pub fn meminfo(text: &str) -> (Option<Memory>, Option<Swap>) {
    let mut values: HashMap<&str, u64> = HashMap::new();
    for line in text.lines() {
        if let Some((key, rest)) = line.split_once(':')
            && let Some(number) = rest.split_whitespace().next().and_then(|n| n.parse::<u64>().ok()) {
                values.insert(key, number * 1024);
            }
    }
    let total = values.get("MemTotal").copied().unwrap_or(0);
    if total == 0 {
        return (None, None);
    }
    let free = values.get("MemFree").copied().unwrap_or(0);
    let available = values.get("MemAvailable").copied().unwrap_or(free);
    let used = total.saturating_sub(available);
    // O que não está em uso nem livre é cache que o kernel devolve quando precisa.
    let cache = total.saturating_sub(used).saturating_sub(free);
    let memory = Memory {
        total_bytes: total,
        used_bytes: used,
        used_pct: crate::store::round(used as f64 / total as f64 * 100.0, 1),
        free_bytes: free,
        parts: vec![
            MemoryPart { label: "Usada".into(), bytes: used, ring: true },
            MemoryPart { label: "Cache".into(), bytes: cache, ring: false },
        ],
    };
    let swap_total = values.get("SwapTotal").copied().unwrap_or(0);
    let swap = (swap_total > 0).then(|| Swap {
        total_mb: crate::store::round(swap_total as f64 / 1048576.0, 1),
        used_mb: crate::store::round(swap_total.saturating_sub(values.get("SwapFree").copied().unwrap_or(0)) as f64 / 1048576.0, 1),
    });
    (Some(memory), swap)
}

/// Rota padrão de `/proc/net/route`: (interface, gateway).
pub fn default_route(text: &str) -> Option<(String, Option<Ipv4Addr>)> {
    let mut best: Option<(u32, String, Option<Ipv4Addr>)> = None;
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 7 || fields[1] != "00000000" {
            continue;
        }
        let metric: u32 = fields[6].parse().unwrap_or(u32::MAX);
        // O arquivo usa a ordem de bytes da máquina; em x86 e ARM é little-endian.
        let gateway = u32::from_str_radix(fields[2], 16).ok().map(|raw| Ipv4Addr::from(raw.swap_bytes()));
        let gateway = gateway.filter(|address| !address.is_unspecified());
        if best.as_ref().is_none_or(|(current, _, _)| metric < *current) {
            best = Some((metric, fields[0].to_string(), gateway));
        }
    }
    best.map(|(_, iface, gateway)| (iface, gateway))
}

pub fn net_dev(text: &str, iface: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|line| {
        let (name, data) = line.split_once(':')?;
        if name.trim() != iface {
            return None;
        }
        let fields: Vec<&str> = data.split_whitespace().collect();
        Some((fields.first()?.parse().ok()?, fields.get(8)?.parse().ok()?))
    })
}

/// Linhas do `journalctl -o json`. Devolve os eventos e o último cursor.
pub fn journal(text: &str) -> (Vec<SshEvent>, Option<String>) {
    let mut events = Vec::new();
    let mut cursor = None;
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if let Some(value) = entry.get("__CURSOR").and_then(|v| v.as_str()) {
            cursor = Some(value.to_string());
        }
        let message = match entry.get("MESSAGE") {
            Some(serde_json::Value::String(text)) => text.clone(),
            // journalctl manda como lista de bytes quando a linha não é UTF-8 válido.
            Some(serde_json::Value::Array(bytes)) => {
                String::from_utf8_lossy(&bytes.iter().filter_map(|b| b.as_u64().map(|b| b as u8)).collect::<Vec<_>>()).into_owned()
            }
            _ => continue,
        };
        let Some(parsed) = ssh::parse(&message) else { continue };
        let Some(stamp) = entry.get("__REALTIME_TIMESTAMP").and_then(|v| v.as_str()).and_then(|v| v.parse::<i64>().ok()) else {
            continue;
        };
        events.push(SshEvent {
            uid: entry.get("__CURSOR").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| stamp.to_string()),
            ts: stamp / 1_000_000,
            kind: parsed.kind.into(),
            method: parsed.method,
            user: parsed.user,
            ip: parsed.ip,
            key: parsed.key,
        });
    }
    (events, cursor)
}

/// `ss -H -lntp`: portas TCP fora do loopback. O nome do processo só aparece para root.
pub fn ss_listeners(text: &str) -> Vec<Listener> {
    let mut found: std::collections::BTreeMap<(u16, String), String> = std::collections::BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let Some(local) = fields.get(3) else { continue };
        let Some((address, port)) = local.rsplit_once(':') else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        // "[fd7a::1]%tailscale0" → "fd7a::1": tira a interface antes dos colchetes.
        let address = address.split('%').next().unwrap_or(address);
        let address = address.trim_matches(|c| c == '[' || c == ']');
        if address.starts_with("127.") || address == "::1" {
            continue;
        }
        let scope = if matches!(address, "0.0.0.0" | "::" | "*") { "todas as interfaces".to_string() } else { address.to_string() };
        let process = line
            .split_once("users:((\"")
            .and_then(|(_, tail)| tail.split_once('"'))
            .map(|(name, _)| name.to_string())
            .unwrap_or_default();
        let entry = found.entry((port, scope)).or_default();
        if entry.is_empty() {
            *entry = process;
        }
    }
    found.into_iter().map(|((port, address), process)| Listener { port, address, process }).collect()
}

/// `ps` com as colunas pcpu, pmem, comm, já ordenado por CPU.
pub fn processes(text: &str, limit: usize) -> Vec<Process> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let cpu = parts.next()?.parse().ok()?;
            let mem = parts.next()?.parse().ok()?;
            let name = parts.collect::<Vec<_>>().join(" ");
            // O próprio `ps` que gerou a lista não interessa.
            (!name.is_empty() && name != "ps").then_some(Process { cpu_pct: cpu, mem_pct: mem, name })
        })
        .take(limit)
        .collect()
}

/// `ip -4 neigh show dev X`: (ip, mac, estado).
pub fn neighbors(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let ip: Ipv4Addr = fields.first()?.parse().ok()?;
            let at = fields.iter().position(|field| *field == "lladdr")?;
            let mac = fields.get(at + 1)?.to_lowercase();
            Some((ip.to_string(), mac, fields.last()?.to_string()))
        })
        .collect()
}

/// Endereços a varrer. Só redes privadas IPv4 de /22 para baixo.
pub fn hosts_to_probe(address: Ipv4Addr, mask: Ipv4Addr) -> Vec<String> {
    let mask_bits = u32::from(mask);
    let prefix = mask_bits.count_ones();
    if !address.is_private() || !(22..=30).contains(&prefix) {
        return Vec::new();
    }
    let network = u32::from(address) & mask_bits;
    let broadcast = network | !mask_bits;
    ((network + 1)..broadcast)
        .map(Ipv4Addr::from)
        .filter(|host| *host != address)
        .map(|host| host.to_string())
        .collect()
}

pub fn oui_table(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (prefix, vendor) = line.trim().split_once("(hex)")?;
            let prefix = prefix.trim();
            (prefix.len() == 8 && prefix.as_bytes()[2] == b'-').then(|| (prefix.replace('-', "").to_uppercase(), vendor.trim().to_string()))
        })
        .collect()
}

pub fn vendor_for(mac: &str, table: &HashMap<String, String>) -> String {
    let compact: String = mac.chars().filter(|c| c.is_ascii_hexdigit()).collect::<String>().to_uppercase();
    compact.get(..6).and_then(|prefix| table.get(prefix)).cloned().unwrap_or_default()
}

pub fn fail2ban(text: &str) -> Option<Fail2ban> {
    let mut result = Fail2ban { jail: String::new(), currently_banned: 0, total_banned: 0, currently_failed: 0 };
    let mut found = false;
    for line in text.lines() {
        if let Some(jail) = line.strip_prefix("Status for the jail:") {
            result.jail = jail.trim().to_string();
            found = true;
        }
        let number = || line.rsplit(':').next().and_then(|tail| tail.split_whitespace().next()).and_then(|n| n.parse().ok()).unwrap_or(0);
        if line.contains("Currently banned:") {
            result.currently_banned = number();
        } else if line.contains("Total banned:") {
            result.total_banned = number();
        } else if line.contains("Currently failed:") {
            result.currently_failed = number();
        }
    }
    found.then_some(result)
}

/// `apt-get -s upgrade`: (total, de segurança).
pub fn pending_upgrades(text: &str) -> (usize, usize) {
    let lines: Vec<&str> = text.lines().filter(|line| line.starts_with("Inst ")).collect();
    let security = lines.iter().filter(|line| line.to_lowercase().contains("security")).count();
    (lines.len(), security)
}

pub fn unattended_enabled(config: &str) -> bool {
    config.lines().any(|line| {
        let line = line.trim();
        line.starts_with("APT::Periodic::Unattended-Upgrade") && line.contains("\"1\"")
    })
}

/// `nft list ruleset`: alguma cadeia de entrada (`hook input`) com política `drop`.
/// A cadeia FORWARD do Docker também tem `policy drop`, mas não protege a máquina.
pub fn nft_input_drops(ruleset: &str) -> bool {
    ruleset.lines().any(|line| {
        let line = line.trim();
        line.contains("hook input") && line.contains("policy drop")
    })
}

pub fn os_pretty_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| line.strip_prefix("PRETTY_NAME=").map(|v| v.trim().trim_matches('"').to_string()))
}

pub fn cpu_model(cpuinfo: &str) -> Option<String> {
    cpuinfo.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        let key = key.trim();
        (key == "model name" || key == "Model" || key == "Hardware")
            .then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu() {
        let before = proc_stat_cpu("cpu  100 0 50 800 10 0 0 0 0 0\ncpu0 1 2 3\n").unwrap();
        let after = proc_stat_cpu("cpu  130 0 60 850 20 0 0 0 0 0\n").unwrap();
        let parts = cpu_parts(&before, &after).unwrap();
        assert_eq!((parts.user, parts.system, parts.iowait, parts.idle), (30.0, 10.0, 10.0, 50.0));
        assert_eq!(boot_time("cpu 1 2\nbtime 1793069247\n"), Some(1793069247));
    }

    #[test]
    fn memory_and_swap() {
        let text = "MemTotal: 1000 kB\nMemFree: 200 kB\nMemAvailable: 600 kB\nSwapTotal: 2048 kB\nSwapFree: 1024 kB\n";
        let (memory, swap) = meminfo(text);
        let memory = memory.unwrap();
        assert_eq!(memory.used_bytes, 400 * 1024);
        assert_eq!(memory.used_pct, 40.0);
        assert_eq!(memory.parts[1].bytes, 400 * 1024);
        assert_eq!(swap.unwrap().used_mb, 1.0);
    }

    #[test]
    fn route_and_counters() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\nenx00\t00000000\t016363OA\t0003\t0\t0\t100\t00000000\nenx00\t00000000\t0163630A\t0003\t0\t0\t50\t00000000\nenx00\t0063630A\t00000000\t0001\t0\t0\t100\t00FFFFFF\n";
        assert_eq!(default_route(route), Some(("enx00".to_string(), Some(Ipv4Addr::new(10, 99, 99, 1)))));
        let dev = "Inter-|   Receive\n face |bytes packets\n  eth0: 1000 5 0 0 0 0 0 0 2000 7 0 0 0 0 0 0\n";
        assert_eq!(net_dev(dev, "eth0"), Some((1000, 2000)));
    }

    #[test]
    fn journal_lines() {
        let lines = [
            r#"{"__CURSOR":"c1","__REALTIME_TIMESTAMP":"1794418802512345","MESSAGE":"Accepted publickey for admin from 10.0.0.1 port 5 ssh2: ED25519 SHA256:k"}"#,
            r#"{"__CURSOR":"c2","__REALTIME_TIMESTAMP":"1794418803000000","MESSAGE":"pam_unix(sshd:session): session opened"}"#,
            "lixo",
        ];
        let (events, cursor) = journal(&lines.join("\n"));
        assert_eq!(cursor.as_deref(), Some("c2"));
        assert_eq!(events.len(), 1);
        assert_eq!((events[0].ts, events[0].uid.as_str(), events[0].key.as_str()), (1794418802, "c1", "SHA256:k"));
    }

    #[test]
    fn listeners_skip_loopback() {
        let text = "LISTEN 0 128 0.0.0.0:22 0.0.0.0:* users:((\"sshd\",pid=1,fd=3))\nLISTEN 0 128 [::]:22 [::]:*\nLISTEN 0 4096 127.0.0.53%lo:53 0.0.0.0:*\nLISTEN 0 4096 100.64.0.10:80 0.0.0.0:* users:((\"host-status\",pid=9,fd=7))\nLISTEN 0 4096 [fd7a:115c::1]%tailscale0:443 [::]:*\n";
        let found = ss_listeners(text);
        assert_eq!(found.len(), 3);
        assert_eq!(found[2].address, "fd7a:115c::1");
        assert_eq!((found[0].port, found[0].address.as_str(), found[0].process.as_str()), (22, "todas as interfaces", "sshd"));
        assert_eq!(found[1].address, "100.64.0.10");
    }

    #[test]
    fn process_table() {
        let rows = processes(" %CPU %MEM COMMAND\n 12.5  3.0 Google Chrome\n  1.0  0.1 zsh\n", 1);
        assert_eq!(rows, vec![Process { cpu_pct: 12.5, mem_pct: 3.0, name: "Google Chrome".into() }]);
    }

    #[test]
    fn lan() {
        let rows = neighbors("192.168.1.1 lladdr 00:00:5E:00:53:01 REACHABLE\n192.168.1.9 FAILED\nfe80::1 lladdr aa:bb STALE\n");
        assert_eq!(rows, vec![("192.168.1.1".into(), "00:00:5e:00:53:01".into(), "REACHABLE".into())]);
        let hosts = hosts_to_probe(Ipv4Addr::new(192, 168, 1, 112), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(hosts.len(), 253);
        assert!(!hosts.contains(&"192.168.1.112".to_string()));
        assert!(hosts_to_probe(Ipv4Addr::new(10, 0, 0, 5), Ipv4Addr::new(255, 0, 0, 0)).is_empty());
        assert!(hosts_to_probe(Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(255, 255, 255, 0)).is_empty());
        let table = oui_table("00-00-5E   (hex)\t\tICANN, IANA Department\n00005E     (base 16)\t\tICANN, IANA Department\n");
        assert_eq!(vendor_for("00:00:5e:00:53:01", &table), "ICANN, IANA Department");
    }

    #[test]
    fn fail2ban_and_apt() {
        let status = "Status for the jail: sshd\n|- Filter\n|  |- Currently failed:\t2\n|  `- Total failed:\t9\n`- Actions\n   |- Currently banned:\t1\n   |- Total banned:\t4\n";
        let parsed = fail2ban(status).unwrap();
        assert_eq!((parsed.currently_banned, parsed.total_banned, parsed.currently_failed), (1, 4, 2));
        assert!(fail2ban("ERROR").is_none());
        let apt = "Inst libc6 [2.35] (2.35-1 Ubuntu:22.04/jammy-security [amd64])\nInst vim [1] (2 Ubuntu:22.04/jammy-updates [amd64])\nConf libc6\n";
        assert_eq!(pending_upgrades(apt), (2, 1));
        assert!(unattended_enabled("APT::Periodic::Update-Package-Lists \"1\";\nAPT::Periodic::Unattended-Upgrade \"1\";\n"));
        assert!(!unattended_enabled("APT::Periodic::Unattended-Upgrade \"0\";\n"));
    }

    #[test]
    fn firewall_policy() {
        let docker_only = "table ip filter {\n\tchain FORWARD {\n\t\ttype filter hook forward priority filter; policy drop;\n\t}\n}\n";
        assert!(!nft_input_drops(docker_only));
        let real = "table inet filter {\n\tchain input {\n\t\ttype filter hook input priority filter; policy drop;\n\t}\n}\n";
        assert!(nft_input_drops(real));
    }

    #[test]
    fn host_text() {
        assert_eq!(os_pretty_name("NAME=x\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n").as_deref(), Some("Debian GNU/Linux 13 (trixie)"));
        assert_eq!(cpu_model("processor: 0\nmodel name\t: Intel(R) Atom(TM)  x5-Z8350\n").as_deref(), Some("Intel(R) Atom(TM) x5-Z8350"));
    }
}
