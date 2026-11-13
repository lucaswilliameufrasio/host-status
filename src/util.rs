//! Comandos com tempo limite, arquivos e chamadas do sistema comuns aos dois sistemas.

use std::io::Read;
use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Roda um comando e devolve a saída padrão, ou None se não existir ou passar do tempo.
/// O stderr é descartado. Um código de saída diferente de zero não é erro: muitos
/// comandos (fping, ufw) usam o código para dizer outra coisa.
pub fn run(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    run_with_status(program, args, timeout).map(|(_, output)| output)
}

pub fn run_ok(program: &str, args: &[&str], timeout: Duration) -> bool {
    matches!(run_with_status(program, args, timeout), Some((true, _)))
}

fn run_with_status(program: &str, args: &[&str], timeout: Duration) -> Option<(bool, String)> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // Lê numa thread para o pipe não encher enquanto esperamos o processo.
    let reader = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout.read_to_end(&mut buffer);
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let bytes = reader.join().ok()?;
    Some((status.success(), String::from_utf8_lossy(&bytes).into_owned()))
}

pub fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn load_average() -> Option<(f64, f64, f64)> {
    let mut values = [0f64; 3];
    // SAFETY: getloadavg escreve até 3 doubles no buffer que passamos.
    let count = unsafe { libc::getloadavg(values.as_mut_ptr(), 3) };
    (count == 3).then_some((values[0], values[1], values[2]))
}

/// (total, livre para usuário comum) em bytes.
pub fn filesystem(path: &str) -> Option<(u64, u64)> {
    let c_path = std::ffi::CString::new(path).ok()?;
    // SAFETY: statvfs preenche a struct zerada; o ponteiro do caminho vive até o fim da chamada.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    Some((stats.f_blocks as u64 * block, stats.f_bavail as u64 * block))
}

/// Endereços IPv4 de cada interface: (interface, endereço, máscara).
pub fn ipv4_addresses() -> Vec<(String, Ipv4Addr, Ipv4Addr)> {
    let mut result = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs aloca a lista; liberamos com freeifaddrs no fim e só lemos
    // sockaddr_in quando a família é AF_INET.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return result;
        }
        let mut cursor = head;
        while !cursor.is_null() {
            let entry = &*cursor;
            if !entry.ifa_addr.is_null() && (*entry.ifa_addr).sa_family as i32 == libc::AF_INET {
                let address = &*(entry.ifa_addr as *const libc::sockaddr_in);
                let mask = if entry.ifa_netmask.is_null() {
                    Ipv4Addr::UNSPECIFIED
                } else {
                    let mask = &*(entry.ifa_netmask as *const libc::sockaddr_in);
                    Ipv4Addr::from(u32::from_be(mask.sin_addr.s_addr))
                };
                let name = std::ffi::CStr::from_ptr(entry.ifa_name).to_string_lossy().into_owned();
                result.push((name, Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr)), mask));
            }
            cursor = entry.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    result
}

/// Faixa da Tailscale: 100.64.0.0/10.
pub fn is_tailscale(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    octets[0] == 100 && (64..=127).contains(&octets[1])
}

pub fn tailscale_ipv4() -> Option<Ipv4Addr> {
    ipv4_addresses().into_iter().map(|(_, address, _)| address).find(|address| is_tailscale(*address))
}

pub fn probe_tcp(host: &str, port: u16) -> bool {
    let Ok(addresses) = (host, port).to_socket_addrs() else { return false };
    addresses
        .take(2)
        .any(|address: SocketAddr| TcpStream::connect_timeout(&address, Duration::from_secs(2)).is_ok())
}

pub fn probe_dns(name: &str) -> bool {
    (name, 443).to_socket_addrs().map(|mut found| found.next().is_some()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_output_and_timeout() {
        assert_eq!(run("echo", &["oi"], Duration::from_secs(2)).as_deref(), Some("oi\n"));
        assert!(run("sleep", &["5"], Duration::from_millis(100)).is_none());
        assert!(run("nao-existe-esse-comando", &[], Duration::from_secs(1)).is_none());
        assert!(run_ok("true", &[], Duration::from_secs(1)));
        assert!(!run_ok("false", &[], Duration::from_secs(1)));
    }

    #[test]
    fn tailscale_range() {
        assert!(is_tailscale(Ipv4Addr::new(100, 75, 199, 42)));
        assert!(!is_tailscale(Ipv4Addr::new(100, 10, 0, 1)));
        assert!(!is_tailscale(Ipv4Addr::new(10, 99, 99, 103)));
    }

    #[test]
    fn system_calls_answer() {
        assert!(load_average().is_some());
        let (total, free) = filesystem("/").unwrap();
        assert!(total > 0 && free <= total);
        assert!(ipv4_addresses().iter().any(|(_, address, _)| address.is_loopback()));
    }
}
