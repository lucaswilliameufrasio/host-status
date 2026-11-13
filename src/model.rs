//! Dados que o coletor produz e a página lê. Os nomes dos campos são o contrato do JSON.

use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CpuPart {
    pub label: String,
    pub pct: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryPart {
    pub label: String,
    pub bytes: u64,
    /// Entra no anel de uso. No Linux o cache aparece na barra, mas não no anel.
    pub ring: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub used_pct: f64,
    pub free_bytes: u64,
    pub parts: Vec<MemoryPart>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Swap {
    pub total_mb: f64,
    pub used_mb: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Disk {
    pub label: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub used_pct: f64,
}

/// Uma leitura rápida: o que a página mostra em "Agora".
#[derive(Debug, Clone, Default, Serialize)]
pub struct Sample {
    pub cpu_pct: Option<f64>,
    pub cpu_parts: Vec<CpuPart>,
    pub cpu_idle: Option<f64>,
    pub temperature_c: Option<f64>,
    pub load1: Option<f64>,
    pub load5: Option<f64>,
    pub load15: Option<f64>,
    pub memory: Option<Memory>,
    pub swap: Option<Swap>,
    pub disk: Option<Disk>,
    pub interface: Option<String>,
    #[serde(skip)]
    pub gateway: Option<String>,
    #[serde(skip)]
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub source_ip: Option<String>,
    #[serde(rename = "rx_total_bytes")]
    pub rx_bytes: Option<u64>,
    #[serde(rename = "tx_total_bytes")]
    pub tx_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Live {
    pub generated_at: i64,
    pub rx_bps: Option<f64>,
    pub tx_bps: Option<f64>,
    #[serde(flatten)]
    pub sample: Sample,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SshEvent {
    pub uid: String,
    pub ts: i64,
    pub kind: String,
    pub method: String,
    pub user: String,
    pub ip: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Ok,
    Atencao,
    Problema,
    Desconhecido,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Check {
    pub id: String,
    pub label: String,
    pub state: State,
    pub detail: String,
}

impl Check {
    pub fn new(id: &str, label: &str, state: State, detail: impl Into<String>) -> Self {
        Check { id: id.to_string(), label: label.to_string(), state, detail: detail.into() }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Listener {
    pub port: u16,
    pub address: String,
    pub process: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Process {
    pub cpu_pct: f64,
    pub mem_pct: f64,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostInfo {
    pub name: String,
    pub role: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub chip: Option<String>,
    pub cores: Option<usize>,
    pub product: Option<String>,
    pub boot_ts: Option<i64>,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Neighbor {
    pub ip: String,
    pub mac: String,
    pub state: String,
    pub vendor: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Fail2ban {
    pub jail: String,
    pub currently_banned: u64,
    pub total_banned: u64,
    pub currently_failed: u64,
}
