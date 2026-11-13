//! Leituras do Linux. O serviço roda como root: journal do sshd, `sshd -T`, fail2ban e o
//! nome do processo de cada porta só aparecem para root. Sem root funciona, com menos dados.

use super::Extras;
use crate::checks;
use crate::config::Config;
use crate::linux as parse;
use crate::model::{Check, CpuPart, Disk, HostInfo, Listener, Neighbor, Process, Sample, SshEvent, State};
use crate::store;
use crate::util::{self, read, run, run_ok};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const NAME: &str = "linux";
const SHORT: Duration = Duration::from_secs(10);
/// `ss -p` e `ps` percorrem todos os processos; com o CPUQuota do unit, numa CPU fraca e
/// com muitos contêineres, passam de 10 s.
const SLOW: Duration = Duration::from_secs(30);
const UPDATES_EVERY: Duration = Duration::from_secs(6 * 3600);

pub struct Sampler {
    previous: Option<[u64; 8]>,
}

impl Sampler {
    pub fn new() -> Self {
        Sampler { previous: None }
    }

    /// Leitura barata: só arquivos do /proc e chamadas do sistema, nenhum processo.
    pub fn sample(&mut self) -> Sample {
        let mut sample = Sample::default();
        let mut current = read("/proc/stat").and_then(|text| parse::proc_stat_cpu(&text));
        if self.previous.is_none() && current.is_some() {
            self.previous = current;
            std::thread::sleep(Duration::from_millis(500));
            current = read("/proc/stat").and_then(|text| parse::proc_stat_cpu(&text));
        }
        if let (Some(previous), Some(now)) = (self.previous, current)
            && let Some(parts) = parse::cpu_parts(&previous, &now) {
                sample.cpu_pct = Some(store::round(100.0 - parts.idle, 1));
                sample.cpu_idle = Some(parts.idle);
                sample.cpu_parts = vec![
                    CpuPart { label: "Usuário".into(), pct: parts.user },
                    CpuPart { label: "Sistema".into(), pct: parts.system },
                    CpuPart { label: "Espera de E/S".into(), pct: parts.iowait },
                ];
            }
        self.previous = current;
        sample.temperature_c = temperature();
        if let Some((one, five, fifteen)) = util::load_average() {
            sample.load1 = Some(store::round(one, 2));
            sample.load5 = Some(store::round(five, 2));
            sample.load15 = Some(store::round(fifteen, 2));
        }
        let (memory, swap) = parse::meminfo(&read("/proc/meminfo").unwrap_or_default());
        sample.memory = memory;
        sample.swap = swap;
        if let Some((total, free)) = util::filesystem("/")
            && total > 0 {
                sample.disk = Some(Disk {
                    label: "Raiz (/)".into(),
                    total_bytes: total,
                    free_bytes: free,
                    used_pct: store::round((total - free) as f64 / total as f64 * 100.0, 1),
                });
            }
        if let Some((iface, gateway)) = read("/proc/net/route").and_then(|text| parse::default_route(&text)) {
            if let Some((rx, tx)) = read("/proc/net/dev").and_then(|text| parse::net_dev(&text, &iface)) {
                sample.rx_bytes = Some(rx);
                sample.tx_bytes = Some(tx);
            }
            sample.source_ip = util::ipv4_addresses()
                .into_iter()
                .find(|(name, _, _)| *name == iface)
                .map(|(_, address, _)| address.to_string());
            sample.gateway = gateway.map(|address| address.to_string());
            sample.interface = Some(iface);
        }
        sample
    }
}

/// Maior temperatura entre os sensores de CPU. None quando a máquina não expõe sensor.
fn temperature() -> Option<f64> {
    const CPU_SENSORS: [&str; 5] = ["coretemp", "k10temp", "zenpower", "cpu_thermal", "acpitz"];
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/sys/class/hwmon") {
        for dir in entries.flatten().map(|entry| entry.path()) {
            if !CPU_SENSORS.contains(&read(dir.join("name")).unwrap_or_default().trim()) {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&dir) {
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    if name.starts_with("temp") && name.ends_with("_input") {
                        paths.push(file.path());
                    }
                }
            }
        }
    }
    if paths.is_empty()
        && let Ok(zones) = std::fs::read_dir("/sys/class/thermal") {
            for zone in zones.flatten().map(|entry| entry.path()) {
                let kind = read(zone.join("type")).unwrap_or_default().to_lowercase();
                if ["x86_pkg", "cpu", "soc", "acpitz"].iter().any(|word| kind.contains(word)) {
                    paths.push(zone.join("temp"));
                }
            }
        }
    paths
        .iter()
        .filter_map(|path| read(path)?.trim().parse::<f64>().ok())
        .map(|millidegrees| millidegrees / 1000.0)
        .filter(|value| *value > 0.0 && *value < 130.0)
        .reduce(f64::max)
        .map(|value| store::round(value, 1))
}

pub fn read_ssh(conn: &Connection) -> (Vec<SshEvent>, Option<String>) {
    let cursor = store::meta_get(conn, "ssh_cursor");
    let mut args = vec!["-o", "json", "--no-pager", "SYSLOG_IDENTIFIER=sshd", "SYSLOG_IDENTIFIER=sshd-session", "SYSLOG_IDENTIFIER=sshd-auth"];
    let after;
    match &cursor {
        Some(value) => {
            after = format!("--after-cursor={value}");
            args.push(&after);
        }
        // Na primeira vez traz o que o journal ainda tiver da última semana.
        None => args.extend(["--since", "-7d"]),
    }
    let Some(output) = run("journalctl", &args, Duration::from_secs(30)) else {
        return (Vec::new(), Some("journalctl não respondeu".into()));
    };
    let (events, next) = parse::journal(&output);
    if let Some(next) = next {
        let _ = store::meta_set(conn, "ssh_cursor", &next);
    }
    // SAFETY: geteuid só lê o id efetivo do processo.
    if output.trim().is_empty() && cursor.is_none() && unsafe { libc::geteuid() } != 0 {
        return (events, Some("sem permissão para ler o journal".into()));
    }
    (events, None)
}

pub fn host_info(config: &Config) -> HostInfo {
    let hostname = read("/proc/sys/kernel/hostname").map(|name| name.trim().to_string()).unwrap_or_default();
    HostInfo {
        name: config.name.clone().unwrap_or(hostname),
        role: config.role.clone(),
        os: read("/etc/os-release").and_then(|text| parse::os_pretty_name(&text)),
        kernel: read("/proc/sys/kernel/osrelease").map(|value| value.trim().to_string()),
        chip: read("/proc/cpuinfo").and_then(|text| parse::cpu_model(&text)),
        // available_parallelism respeita o CPUQuota do systemd e diria 1; aqui vale o hardware.
        // SAFETY: sysconf só lê uma configuração do sistema.
        cores: usize::try_from(unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }).ok().filter(|n| *n > 0),
        product: read("/sys/class/dmi/id/product_name").map(|v| v.trim().to_string()).filter(|v| !v.is_empty()),
        boot_ts: read("/proc/stat").and_then(|text| parse::boot_time(&text)),
    }
}

fn firewall() -> Check {
    let ufw = run("ufw", &["status"], SHORT);
    if ufw.as_deref().is_some_and(|text| text.contains("Status: active")) {
        return Check::new("firewall", "Firewall", State::Ok, "ufw ativo");
    }
    if run_ok("systemctl", &["is-active", "--quiet", "firewalld"], SHORT) {
        return Check::new("firewall", "Firewall", State::Ok, "firewalld ativo");
    }
    let nft = run("nft", &["list", "ruleset"], SHORT);
    if nft.as_deref().is_some_and(parse::nft_input_drops) {
        return Check::new("firewall", "Firewall", State::Ok, "nftables com política de bloqueio");
    }
    // SAFETY: geteuid só lê o id efetivo do processo.
    if unsafe { libc::geteuid() } != 0 {
        return Check::new("firewall", "Firewall", State::Desconhecido, "precisa de root para ler");
    }
    Check::new("firewall", "Firewall", State::Atencao, "nenhum ativo; a proteção fica com o roteador e a Tailscale")
}

fn automatic_updates() -> Check {
    if !Path::new("/usr/bin/unattended-upgrade").exists() {
        return Check::new("auto_updates", "Atualizações automáticas", State::Atencao, "unattended-upgrades não instalado");
    }
    let mut config = String::new();
    for file in ["/etc/apt/apt.conf.d/20auto-upgrades", "/etc/apt/apt.conf.d/52unattended-upgrades-local"] {
        config.push_str(&read(file).unwrap_or_default());
    }
    if parse::unattended_enabled(&config) {
        Check::new("auto_updates", "Atualizações automáticas", State::Ok, "unattended-upgrades ligado")
    } else {
        Check::new("auto_updates", "Atualizações automáticas", State::Atencao, "unattended-upgrades desligado")
    }
}

fn check_updates() -> Check {
    match run("apt-get", &["-s", "-o", "Debug::NoLocking=1", "upgrade"], Duration::from_secs(300)) {
        None => Check::new("updates", "Atualizações pendentes", State::Desconhecido, "apt não respondeu em 5 minutos"),
        Some(output) => match parse::pending_upgrades(&output) {
            (0, _) => Check::new("updates", "Atualizações pendentes", State::Ok, "nenhuma"),
            (total, 0) => Check::new("updates", "Atualizações pendentes", State::Atencao, total.to_string()),
            (total, security) => Check::new("updates", "Atualizações pendentes", State::Atencao, format!("{total}, {security} de segurança")),
        },
    }
}

/// O `apt-get -s` leva minutos numa CPU fraca: roda numa thread própria a cada 6 h e a
/// coleta usa o último resultado, sem esperar.
fn pending_updates() -> Check {
    struct Cache {
        result: Option<(Instant, Check)>,
        running: bool,
    }
    static CACHE: Mutex<Cache> = Mutex::new(Cache { result: None, running: false });
    let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
    let stale = cache.result.as_ref().is_none_or(|(when, _)| when.elapsed() >= UPDATES_EVERY);
    if stale && !cache.running {
        cache.running = true;
        std::thread::spawn(|| {
            let check = check_updates();
            let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
            cache.result = Some((Instant::now(), check));
            cache.running = false;
        });
    }
    match &cache.result {
        Some((_, check)) => check.clone(),
        None => Check::new("updates", "Atualizações pendentes", State::Desconhecido, "verificando"),
    }
}

pub fn posture(_config: &Config) -> Vec<Check> {
    let mut list = Vec::new();
    match run("/usr/sbin/sshd", &["-T"], SHORT).filter(|text| !text.trim().is_empty()) {
        Some(text) => list.extend(checks::sshd_checks(&checks::sshd_settings(&text))),
        None => list.push(Check::new("ssh_senha", "SSH sem senha", State::Desconhecido, "sshd -T precisa de root")),
    }
    list.push(firewall());
    match run("fail2ban-client", &["status", "sshd"], SHORT).and_then(|text| parse::fail2ban(&text)) {
        Some(status) => list.push(Check::new("fail2ban", "fail2ban", State::Ok, format!("ativo, {} banido(s) agora", status.currently_banned))),
        None if Path::new("/usr/bin/fail2ban-client").exists() => {
            list.push(Check::new("fail2ban", "fail2ban", State::Atencao, "instalado, sem resposta da jail sshd"))
        }
        None => list.push(Check::new("fail2ban", "fail2ban", State::Atencao, "não instalado")),
    }
    if Path::new("/usr/bin/apt-get").exists() {
        list.push(automatic_updates());
        list.push(pending_updates());
    }
    if Path::new("/run/reboot-required").exists() || Path::new("/var/run/reboot-required").exists() {
        list.push(Check::new("reboot", "Reinício pendente", State::Atencao, "uma atualização pede reinício"));
    }
    list
}

pub fn listeners() -> Vec<Listener> {
    parse::ss_listeners(&run("ss", &["-H", "-lntp"], SLOW).unwrap_or_default())
}

pub fn processes() -> Vec<Process> {
    parse::processes(&run("ps", &["-eo", "pcpu,pmem,comm", "--sort=-pcpu"], SLOW).unwrap_or_default(), 6)
}

pub fn default_acari() -> Option<std::path::PathBuf> {
    None
}

pub fn tailscale_state() -> String {
    match util::tailscale_ipv4() {
        Some(address) => format!("conectado, {address}"),
        None if Path::new("/usr/sbin/tailscaled").exists() || Path::new("/usr/bin/tailscale").exists() => {
            "instalado, desconectado".into()
        }
        None => "não instalado".into(),
    }
}

pub fn ping(host: &str) -> bool {
    run_ok("ping", &["-c", "1", "-W", "2", host], Duration::from_secs(4))
}

fn oui() -> &'static HashMap<String, String> {
    static TABLE: OnceLock<HashMap<String, String>> = OnceLock::new();
    TABLE.get_or_init(|| {
        ["/usr/share/ieee-data/oui.txt", "/var/lib/ieee-data/oui.txt"]
            .iter()
            .find_map(read)
            .map(|text| parse::oui_table(&text))
            .unwrap_or_default()
    })
}

fn scan_devices(conn: &Connection, now: i64, sample: &Sample, config: &Config) -> Option<Vec<store::Device>> {
    let iface = sample.interface.as_deref()?;
    let source = sample.source_ip.as_deref()?;
    let (_, address, mask) = util::ipv4_addresses().into_iter().find(|(name, address, _)| name == iface && address.to_string() == source)?;
    let hosts = parse::hosts_to_probe(address, mask);
    if !hosts.is_empty() {
        // O fping só acorda a tabela ARP; a lista vem do `ip neigh`.
        let mut args: Vec<&str> = vec!["-a", "-q", "-i", "5", "-r", "0", "-t", "500", "-I", iface];
        args.extend(hosts.iter().map(String::as_str));
        let _ = run("fping", &args, Duration::from_secs(40));
    }
    let table = oui();
    let gateway = sample.gateway.clone().unwrap_or_default();
    let mut connected = HashSet::new();
    let neighbors: Vec<Neighbor> = parse::neighbors(&run("ip", &["-4", "neigh", "show", "dev", iface], SHORT).unwrap_or_default())
        .into_iter()
        .filter(|(_, _, state)| state != "FAILED" && state != "INCOMPLETE")
        .map(|(ip, mac, state)| {
            if matches!(state.as_str(), "REACHABLE" | "DELAY" | "PROBE" | "PERMANENT") {
                connected.insert(mac.clone());
            }
            let label = config.labels.get(&mac).cloned().unwrap_or_else(|| if ip == gateway { "Gateway".into() } else { String::new() });
            Neighbor { vendor: parse::vendor_for(&mac, table), label, ip, mac, state }
        })
        .collect();
    store::record_devices(conn, now, &neighbors).ok()?;
    store::devices_view(conn, &connected).ok()
}

pub fn extras(conn: &Connection, now: i64, sample: &Sample, config: &Config) -> Extras {
    Extras {
        fail2ban: run("fail2ban-client", &["status", "sshd"], SHORT).and_then(|text| parse::fail2ban(&text)),
        devices: if config.lan_devices { scan_devices(conn, now, sample, config) } else { None },
    }
}

/// Leituras cruas, para depurar numa máquina nova.
pub fn diagnose() -> Vec<String> {
    vec![format!("/proc/stat cpu: {:?}", read("/proc/stat").and_then(|text| parse::proc_stat_cpu(&text)))]
}
