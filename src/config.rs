//! Configuração do host: um JSON pequeno, opcional.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Service {
    pub label: String,
    pub port: u16,
    /// Endereço a testar. "tailscale" usa o IP da Tailscale da própria máquina.
    #[serde(default = "loopback")]
    pub host: String,
}

fn loopback() -> String {
    "127.0.0.1".to_string()
}

fn default_bind() -> Vec<String> {
    vec!["127.0.0.1:8787".to_string()]
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Varre a rede local (fping + ARP) e lista os aparelhos. Só Linux.
    #[serde(default)]
    pub lan_devices: bool,
    /// MAC (minúsculo) → nome mostrado na lista de aparelhos.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub services: Vec<Service>,
    /// Endereços da página: "127.0.0.1:8787", "tailscale:80".
    #[serde(default = "default_bind")]
    pub bind: Vec<String>,
    /// Caminho do acari para o card de limpeza. Sem ele, o card some.
    #[serde(default)]
    pub acari: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str("{}").expect("config vazia é válida")
    }
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let config = Config::default();
        assert_eq!(config.bind, vec!["127.0.0.1:8787"]);
        assert!(!config.lan_devices);
    }

    #[test]
    fn full() {
        let config: Config = serde_json::from_str(r#"{
            "name": "servidor-casa", "role": "Escritório", "lan_devices": true,
            "labels": {"00:00:5e:00:53:02": "Roteador"},
            "services": [{"label": "go2rtc", "port": 1984, "host": "tailscale"}, {"label": "DNS", "port": 53}],
            "bind": ["127.0.0.1:80", "tailscale:80"]
        }"#).unwrap();
        assert_eq!(config.services[1].host, "127.0.0.1");
        assert_eq!(config.labels.get("00:00:5e:00:53:02").unwrap(), "Roteador");
    }

    #[test]
    fn rejects_typos() {
        assert!(serde_json::from_str::<Config>(r#"{"lan_device": true}"#).is_err());
    }
}
