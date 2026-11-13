//! Leituras do macOS. Roda como o usuário da sessão, sem sudo: o log unificado, o firewall
//! e o launchd respondem a quem está no grupo admin.

use super::Extras;
use crate::config::Config;
use crate::macos as parse;
use crate::model::{Check, CpuPart, Disk, HostInfo, Listener, Process, Sample, SshEvent, Swap};
use crate::store;
use crate::util::{self, read, run, run_ok};
use rusqlite::Connection;
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const NAME: &str = "macos";
const SHORT: Duration = Duration::from_secs(10);
const ROUTE_EVERY: Duration = Duration::from_secs(60);
const SSH_LOOKBACK_SECONDS: i64 = 10 * 60;
const REMOTE_TOOLS: [&str; 3] = ["AnyDesk", "RustDesk", "TeamViewer"];
const SSH_PREDICATE: &str = "(process == \"sshd\" OR process == \"sshd-session\" OR process == \"sshd-auth\") AND (\
eventMessage BEGINSWITH \"Accepted \" OR eventMessage BEGINSWITH \"Failed \" OR \
eventMessage BEGINSWITH \"Invalid user \" OR \
(eventMessage BEGINSWITH \"Connection closed by \" AND eventMessage ENDSWITH \"[preauth]\"))";

unsafe extern "C" {
    /// Porta do host para as chamadas host_statistics. O libc marca a dele como obsoleta.
    fn mach_host_self() -> libc::mach_port_t;
}

fn sysctl_bytes(name: &str) -> Option<Vec<u8>> {
    let name = CString::new(name).ok()?;
    let mut size: libc::size_t = 0;
    // SAFETY: primeira chamada só pergunta o tamanho; a segunda escreve até `size` bytes no buffer.
    unsafe {
        if libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 || size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size];
        if libc::sysctlbyname(name.as_ptr(), buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        buffer.truncate(size);
        Some(buffer)
    }
}

fn sysctl_u64(name: &str) -> Option<u64> {
    let bytes = sysctl_bytes(name)?;
    match bytes.len() {
        8 => Some(u64::from_ne_bytes(bytes.try_into().ok()?)),
        4 => Some(u32::from_ne_bytes(bytes.try_into().ok()?) as u64),
        _ => None,
    }
}

fn sysctl_string(name: &str) -> Option<String> {
    let bytes = sysctl_bytes(name)?;
    let text = String::from_utf8_lossy(&bytes);
    Some(text.trim_end_matches('\0').trim().to_string()).filter(|text| !text.is_empty())
}

fn cpu_ticks() -> Option<[u32; 4]> {
    let mut info: libc::host_cpu_load_info = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: o kernel escreve `count` inteiros na struct do tamanho certo.
    let result = unsafe {
        libc::host_statistics(mach_host_self(), libc::HOST_CPU_LOAD_INFO, (&mut info as *mut libc::host_cpu_load_info).cast(), &mut count)
    };
    (result == 0).then_some(info.cpu_ticks)
}

fn vm_statistics() -> Option<libc::vm_statistics64> {
    let mut info: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: o kernel escreve `count` inteiros na struct do tamanho certo.
    let result = unsafe {
        libc::host_statistics64(mach_host_self(), libc::HOST_VM_INFO64, (&mut info as *mut libc::vm_statistics64).cast(), &mut count)
    };
    (result == 0).then_some(info)
}

fn swap() -> Option<Swap> {
    let bytes = sysctl_bytes("vm.swapusage")?;
    if bytes.len() < std::mem::size_of::<libc::xsw_usage>() {
        return None;
    }
    // SAFETY: o buffer tem o tamanho da struct; leitura sem alinhamento garantido.
    let usage: libc::xsw_usage = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast()) };
    Some(Swap {
        total_mb: store::round(usage.xsu_total as f64 / 1048576.0, 1),
        used_mb: store::round(usage.xsu_used as f64 / 1048576.0, 1),
    })
}

/// Bytes recebidos e enviados da interface, em 64 bits (NET_RT_IFLIST2).
fn interface_bytes(interface: &str) -> Option<(u64, u64)> {
    let name = CString::new(interface).ok()?;
    // SAFETY: if_nametoindex só lê a string terminada em zero.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) } as u16;
    if index == 0 {
        return None;
    }
    let mut mib = [libc::CTL_NET, libc::PF_ROUTE, 0, 0, libc::NET_RT_IFLIST2, 0];
    let mut size: libc::size_t = 0;
    // SAFETY: mesma dança do sysctl: tamanho primeiro, depois os dados no buffer.
    unsafe {
        if libc::sysctl(mib.as_mut_ptr(), 6, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let mut buffer = vec![0u8; size];
        if libc::sysctl(mib.as_mut_ptr(), 6, buffer.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) != 0 {
            return None;
        }
        let mut offset = 0usize;
        while offset + std::mem::size_of::<libc::if_msghdr2>() <= size {
            let header: libc::if_msghdr2 = std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast());
            if header.ifm_msglen == 0 {
                break;
            }
            if header.ifm_type as i32 == libc::RTM_IFINFO2 && header.ifm_index == index {
                return Some((header.ifm_data.ifi_ibytes, header.ifm_data.ifi_obytes));
            }
            offset += header.ifm_msglen as usize;
        }
    }
    None
}

/// O contador de bytes do NET_RT_IFLIST2 chega truncado em 32 bits. Uma vez por minuto o
/// `netstat -ibn` dá o total real; entre uma leitura e outra soma-se a diferença de 32 bits.
struct Counters {
    rx_total: u64,
    tx_total: u64,
    rx_low: u32,
    tx_low: u32,
}

pub struct Sampler {
    previous: Option<[u32; 4]>,
    route: Option<(Instant, Option<String>, Option<String>)>,
    counters: Option<Counters>,
}

impl Sampler {
    pub fn new() -> Self {
        Sampler { previous: None, route: None, counters: None }
    }

    pub fn sample(&mut self) -> Sample {
        let mut sample = Sample::default();
        let mut current = cpu_ticks();
        if self.previous.is_none() && current.is_some() {
            self.previous = current;
            std::thread::sleep(Duration::from_millis(500));
            current = cpu_ticks();
        }
        if let (Some(before), Some(now)) = (self.previous, current) {
            // user, system, idle, nice; contadores de 32 bits que dão a volta.
            let delta: Vec<f64> = (0..4).map(|i| now[i].wrapping_sub(before[i]) as f64).collect();
            let total: f64 = delta.iter().sum();
            if total > 0.0 {
                let pct = |value: f64| store::round(value / total * 100.0, 1);
                let idle = pct(delta[2]);
                sample.cpu_pct = Some(store::round(100.0 - idle, 1));
                sample.cpu_idle = Some(idle);
                sample.cpu_parts = vec![
                    CpuPart { label: "Usuário".into(), pct: pct(delta[0] + delta[3]) },
                    CpuPart { label: "Sistema".into(), pct: pct(delta[1]) },
                ];
            }
        }
        self.previous = current;
        if let Some((one, five, fifteen)) = util::load_average() {
            sample.load1 = Some(store::round(one, 2));
            sample.load5 = Some(store::round(five, 2));
            sample.load15 = Some(store::round(fifteen, 2));
        }
        if let (Some(total), Some(page), Some(vm)) = (sysctl_u64("hw.memsize"), sysctl_u64("hw.pagesize"), vm_statistics()) {
            sample.memory = parse::memory(total, page, vm.active_count as u64, vm.wire_count as u64, vm.compressor_page_count as u64);
        }
        sample.swap = swap();
        if let Some((total, free)) = util::filesystem("/System/Volumes/Data") {
            if total > 0 {
                sample.disk = Some(Disk {
                    label: "Macintosh HD".into(),
                    total_bytes: total,
                    free_bytes: free,
                    used_pct: store::round((total - free) as f64 / total as f64 * 100.0, 1),
                });
            }
        }
        // A rota padrão só sai de um comando; muda pouco, então vale por um minuto.
        if self.route.as_ref().is_none_or(|(when, _, _)| when.elapsed() >= ROUTE_EVERY) {
            let (interface, gateway) = parse::route(&run("route", &["-n", "get", "default"], SHORT).unwrap_or_default());
            self.counters = interface.as_deref().and_then(|name| {
                let (rx_total, tx_total) = parse::netstat_bytes(&run("netstat", &["-ibn"], SHORT)?, name)?;
                let (rx_low, tx_low) = interface_bytes(name)?;
                Some(Counters { rx_total, tx_total, rx_low: rx_low as u32, tx_low: tx_low as u32 })
            });
            self.route = Some((Instant::now(), interface, gateway));
        }
        if let Some((_, interface, gateway)) = &self.route {
            if let (Some(name), Some(base)) = (interface, &self.counters) {
                if let Some((rx, tx)) = interface_bytes(name) {
                    sample.rx_bytes = Some(base.rx_total + (rx as u32).wrapping_sub(base.rx_low) as u64);
                    sample.tx_bytes = Some(base.tx_total + (tx as u32).wrapping_sub(base.tx_low) as u64);
                }
            }
            sample.interface = interface.clone();
            sample.gateway = gateway.clone();
        }
        sample
    }
}

fn local_time(seconds: i64) -> String {
    let mut time: libc::tm = unsafe { std::mem::zeroed() };
    let value = seconds as libc::time_t;
    // SAFETY: localtime_r escreve na struct que passamos.
    unsafe { libc::localtime_r(&value, &mut time) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", time.tm_year + 1900, time.tm_mon + 1, time.tm_mday, time.tm_hour, time.tm_min, time.tm_sec)
}

/// As mensagens do sshd são de nível Info e somem rápido do log; lê os últimos 10 minutos
/// a cada coleta e o banco guarda.
pub fn read_ssh(_conn: &Connection) -> (Vec<SshEvent>, Option<String>) {
    let start = local_time(util::now() - SSH_LOOKBACK_SECONDS);
    match run("log", &["show", "--start", &start, "--info", "--style", "ndjson", "--predicate", SSH_PREDICATE], Duration::from_secs(45)) {
        Some(output) => (parse::log_events(&output), None),
        None => (Vec::new(), Some("o comando log não respondeu".into())),
    }
}

pub fn host_info(config: &Config) -> HostInfo {
    let name = config
        .name
        .clone()
        .or_else(|| run("scutil", &["--get", "ComputerName"], SHORT).map(|n| n.trim().to_string()).filter(|n| !n.is_empty()))
        .or_else(|| sysctl_string("kern.hostname"))
        .unwrap_or_default();
    let boot_ts = sysctl_bytes("kern.boottime").and_then(|bytes| bytes.get(..8).map(|seconds| i64::from_ne_bytes(seconds.try_into().unwrap_or([0; 8]))));
    HostInfo {
        name,
        role: config.role.clone(),
        os: run("sw_vers", &["-productVersion"], SHORT).map(|v| format!("macOS {}", v.trim())),
        kernel: sysctl_string("kern.osrelease"),
        chip: sysctl_string("machdep.cpu.brand_string"),
        cores: sysctl_u64("hw.ncpu").map(|n| n as usize),
        product: sysctl_string("hw.model"),
        boot_ts,
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

pub fn posture(_config: &Config) -> Vec<Check> {
    let running = run("ps", &["-Aco", "comm"], SHORT).unwrap_or_default();
    let facts = parse::Facts {
        hardening: read("/etc/ssh/sshd_config.d/000-hardening.conf"),
        authorized_keys: read(home().join(".ssh/authorized_keys"))
            .map(|text| text.lines().filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#')).count()),
        launchd: run("launchctl", &["print-disabled", "system"], SHORT).map(|text| parse::launchctl_disabled(&text)),
        firewall: run("/usr/libexec/ApplicationFirewall/socketfilterfw", &["--getglobalstate", "--getstealthmode"], SHORT),
        filevault: run("fdesetup", &["status"], SHORT),
        sip: run("csrutil", &["status"], SHORT),
        remote_tools: REMOTE_TOOLS
            .iter()
            .filter(|name| Path::new(&format!("/Applications/{name}.app")).exists())
            .map(|name| (name.to_string(), running.lines().any(|line| line.trim() == *name)))
            .collect(),
    };
    parse::posture(&facts)
}

/// Serviços do sistema que rodam como root e o `lsof` do usuário não enxerga. A porta 22
/// fica de fora: abrir conexão nela gera "Connection closed [preauth]" no log do sshd.
const SYSTEM_PORTS: [(u16, &str); 5] = [
    (88, "Kerberos"),
    (445, "Compartilhamento de arquivos (SMB)"),
    (548, "AFP"),
    (3283, "Gerenciamento remoto (ARD)"),
    (5900, "Compartilhamento de tela"),
];

pub fn listeners() -> Vec<Listener> {
    // O netstat chamado por um binário fora da Apple não lista sockets TCP no macOS;
    // o lsof funciona, mas só para processos do usuário.
    let mut found = parse::lsof_listeners(&run("lsof", &["-nP", "-iTCP", "-sTCP:LISTEN"], SHORT).unwrap_or_default());
    let remote_login = run("launchctl", &["print-disabled", "system"], SHORT)
        .map(|text| parse::launchctl_disabled(&text))
        .is_some_and(|services| services.get("com.openssh.sshd") == Some(&false));
    if remote_login {
        found.push(Listener { port: 22, address: "todas as interfaces".into(), process: "serviço do sistema: SSH (Login remoto)".into() });
    }
    for (port, label) in SYSTEM_PORTS {
        if !found.iter().any(|item| item.port == port) && util::probe_tcp("127.0.0.1", port) {
            found.push(Listener { port, address: "não identificado".into(), process: format!("serviço do sistema: {label}") });
        }
    }
    found.sort_by_key(|item| item.port);
    found
}

pub fn processes() -> Vec<Process> {
    crate::linux::processes(&run("ps", &["-Aceo", "pcpu,pmem,comm", "-r"], SHORT).unwrap_or_default(), 6)
}

pub fn tailscale_state() -> String {
    match util::tailscale_ipv4() {
        Some(address) => format!("conectado, {address}"),
        None if Path::new("/Applications/Tailscale.app").exists() => "instalado, desconectado".into(),
        None => "não instalado".into(),
    }
}

pub fn ping(host: &str) -> bool {
    run_ok("ping", &["-c", "1", "-t", "2", host], Duration::from_secs(4))
}

pub fn default_acari() -> Option<PathBuf> {
    Some(home().join(".local/bin/acari"))
}

pub fn extras(_conn: &Connection, _now: i64, _sample: &Sample, _config: &Config) -> Extras {
    Extras::default()
}

/// Leituras cruas, para depurar numa máquina nova.
pub fn diagnose() -> Vec<String> {
    let mut sampler = Sampler::new();
    let sample = sampler.sample();
    vec![
        format!("cpu: {:?} {:?}", sample.cpu_pct, sample.cpu_parts),
        format!("interface: {:?}, bytes: {:?} / {:?}", sample.interface, sample.rx_bytes, sample.tx_bytes),
        format!("listeners: {}", listeners().len()),

    ]
}

/// O macOS não tem systemd: não se aplica.
pub fn failed_units() -> Option<Vec<String>> {
    None
}

pub fn oom_kills() -> Option<Vec<(String, i64)>> {
    None
}

/// Roda como o usuário (LaunchAgent): só as chaves dele.
pub fn key_labels() -> std::collections::HashMap<String, String> {
    std::env::var_os("HOME").map(|home| util::key_labels(&[PathBuf::from(home)])).unwrap_or_default()
}
