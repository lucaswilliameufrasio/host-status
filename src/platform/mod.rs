//! O que cada sistema fornece ao coletor.

use crate::model::Fail2ban;
use crate::store::Device;

#[derive(Debug, Default)]
pub struct Extras {
    pub fail2ban: Option<Fail2ban>,
    pub devices: Option<Vec<Device>>,
}

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("o host-status compila para Linux e macOS");
