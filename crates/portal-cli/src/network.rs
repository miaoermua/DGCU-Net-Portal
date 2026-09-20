//! Cross-platform local network-interface discovery.
//!
//! This module reads only the interface selected by the user (or the best
//! active non-loopback interface when no selection has been saved). It does
//! not collect traffic counters, routes, or data from other interfaces.
use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use serde::Serialize;
use std::net::Ipv4Addr;
#[cfg(target_os = "linux")]
use std::path::Path;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct InterfaceInfo {
    pub name: String,
    pub ipv4: Option<String>,
    pub mac: Option<String>,
    pub internal: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkContext {
    pub interface_name: String,
    pub ipv4: String,
    pub mac: String,
}

fn ipv4(interface: &NetworkInterface) -> Option<Ipv4Addr> {
    interface.addr.iter().find_map(|address| match address {
        Addr::V4(value) if !value.ip.is_loopback() => Some(value.ip),
        _ => None,
    })
}

fn normalize_mac(value: &str) -> Option<String> {
    let mac = value.trim().to_ascii_lowercase();
    let valid = mac.len() == 17
        && mac.as_bytes().iter().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    valid
        .then_some(mac)
        .filter(|value| value != "00:00:00:00:00:00" && value != "02:00:00:00:00:00")
}

#[cfg(target_os = "linux")]
fn fallback_mac(name: &str) -> Option<String> {
    let path = Path::new("/sys/class/net").join(name).join("address");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| normalize_mac(&value))
}

#[cfg(target_os = "macos")]
fn fallback_mac(name: &str) -> Option<String> {
    let output = std::process::Command::new("/sbin/ifconfig")
        .arg(name)
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    text.split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|parts| {
            (parts[0] == "ether")
                .then(|| normalize_mac(parts[1]))
                .flatten()
        })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fallback_mac(_name: &str) -> Option<String> {
    None
}

pub fn list() -> Result<Vec<InterfaceInfo>, String> {
    let mut result = NetworkInterface::show()
        .map_err(|error| format!("无法读取系统网卡：{error}"))?
        .into_iter()
        .map(|interface| {
            let ipv4 = ipv4(&interface).map(|value| value.to_string());
            let mac = interface
                .mac_addr
                .as_deref()
                .and_then(normalize_mac)
                .or_else(|| fallback_mac(&interface.name));
            InterfaceInfo {
                name: interface.name,
                ipv4,
                mac,
                internal: interface.internal,
            }
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

/// Resolve a selected interface. An empty name chooses the first active,
/// externally reachable interface with both an IPv4 address and a MAC.
pub fn resolve(selected: &str) -> Result<Option<NetworkContext>, String> {
    let interfaces = list()?;
    let candidate = if selected.trim().is_empty() {
        interfaces
            .iter()
            .find(|item| !item.internal && item.ipv4.is_some() && item.mac.is_some())
    } else {
        interfaces.iter().find(|item| item.name == selected.trim())
    };
    let Some(item) = candidate else {
        if selected.trim().is_empty() {
            return Ok(None);
        }
        return Err(format!("找不到网卡：{}", selected.trim()));
    };
    let Some(ipv4) = item.ipv4.clone() else {
        return Err(format!("网卡 {} 没有 IPv4 地址", item.name));
    };
    let Some(mac) = item.mac.clone() else {
        return Err(format!("网卡 {} 没有可用 MAC 地址", item.name));
    };
    Ok(Some(NetworkContext {
        interface_name: item.name.clone(),
        ipv4,
        mac,
    }))
}

#[cfg(test)]
mod tests {
    use super::normalize_mac;

    #[test]
    fn accepts_only_colon_separated_mac_addresses() {
        assert_eq!(
            normalize_mac("AA:bb:01:02:03:ff"),
            Some("aa:bb:01:02:03:ff".into())
        );
        assert!(normalize_mac("aabbccddeeff").is_none());
        assert!(normalize_mac("00:00:00:00:00").is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_fallback_reads_ifconfig_mac() {
        let output = std::process::Command::new("/sbin/ifconfig")
            .arg("en0")
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        let mac = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find_map(|parts| {
                (parts[0] == "ether")
                    .then(|| normalize_mac(parts[1]))
                    .flatten()
            });
        assert_eq!(mac, super::fallback_mac("en0"));
    }
}
