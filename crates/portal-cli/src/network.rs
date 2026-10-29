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
    let current = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find_map(|parts| {
            (parts[0] == "ether")
                .then(|| normalize_mac(parts[1]))
                .flatten()
        });
    current.or_else(|| {
        let output = std::process::Command::new("/usr/sbin/networksetup")
            .args(["-getmacaddress", name])
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        text.split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find_map(|parts| {
                (parts[0] == "Address:")
                    .then(|| normalize_mac(parts[1]))
                    .flatten()
            })
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn fallback_mac(_name: &str) -> Option<String> {
    None
}

/// 网卡枚举的原始结果：MAC 仍是系统上报的原值，尚未做 `fallback_mac`。
///
/// `list()` 要把每条网卡都交出去（GUI 展示用），所以 MAC 必须一次算全；
/// `resolve()` 只挑一条，没理由为落选的网卡付 fallback 的代价——macOS 上那是
/// `ifconfig` + `networksetup` 两个子进程，本机实测每次 `resolve` 因此要 0.35 秒。
struct RawInterface {
    name: String,
    ipv4: Option<String>,
    mac: Option<String>,
    internal: bool,
}

/// 读取系统网卡并按名字排序，顺序与旧 `list()` 的输出一致。
fn enumerate() -> Result<Vec<RawInterface>, String> {
    let mut interfaces = NetworkInterface::show()
        .map_err(|error| format!("无法读取系统网卡：{error}"))?
        .into_iter()
        .map(|interface| RawInterface {
            ipv4: ipv4(&interface).map(|value| value.to_string()),
            mac: interface.mac_addr,
            internal: interface.internal,
            name: interface.name,
        })
        .collect::<Vec<_>>();
    interfaces.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(interfaces)
}

/// 单条网卡可用的 MAC：系统上报值有效就直用，无效才走平台 fallback。
fn usable_mac(interface: &RawInterface) -> Option<String> {
    interface
        .mac
        .as_deref()
        .and_then(normalize_mac)
        .or_else(|| fallback_mac(&interface.name))
}

pub fn list() -> Result<Vec<InterfaceInfo>, String> {
    Ok(enumerate()?
        .into_iter()
        .map(|interface| {
            let mac = usable_mac(&interface);
            InterfaceInfo {
                name: interface.name,
                ipv4: interface.ipv4,
                mac,
                internal: interface.internal,
            }
        })
        .collect())
}

/// 按旧语义挑一条网卡。MAC 由 `usable_mac` 按需解析：只有真正参与判定的候选
/// 才会触发 fallback，命中之后不再为剩下的网卡付这笔钱。
fn select<F>(
    interfaces: &[RawInterface],
    selected: &str,
    mut usable_mac: F,
) -> Result<Option<NetworkContext>, String>
where
    F: FnMut(&RawInterface) -> Option<String>,
{
    let wanted = selected.trim();
    if wanted.is_empty() {
        // 旧实现的候选顺序是 `list()` 按名字排序后的顺序：第一个「非虚拟 + 有
        // IPv4 + 有可用 MAC」的网卡。这里自己排一次序，使选择结果不依赖调用方
        // 传入的顺序（也只有这一处需要顺序）。
        let mut order = interfaces.iter().collect::<Vec<_>>();
        order.sort_by(|left, right| left.name.cmp(&right.name));
        for interface in order {
            if interface.internal {
                continue;
            }
            let Some(ipv4) = interface.ipv4.clone() else {
                continue;
            };
            // 只有当候选走到这一步、需要判定「有没有可用 MAC」时才解析它。
            let Some(mac) = usable_mac(interface) else {
                continue;
            };
            return Ok(Some(NetworkContext {
                interface_name: interface.name.clone(),
                ipv4,
                mac,
            }));
        }
        return Ok(None);
    }
    let Some(interface) = interfaces.iter().find(|item| item.name == wanted) else {
        return Err(format!("找不到网卡：{wanted}"));
    };
    let Some(ipv4) = interface.ipv4.clone() else {
        return Err(format!("网卡 {} 没有 IPv4 地址", interface.name));
    };
    let Some(mac) = usable_mac(interface) else {
        return Err(format!("网卡 {} 没有可用 MAC 地址", interface.name));
    };
    Ok(Some(NetworkContext {
        interface_name: interface.name.clone(),
        ipv4,
        mac,
    }))
}

/// Resolve a selected interface. An empty name chooses the first active,
/// externally reachable interface with both an IPv4 address and a MAC.
pub fn resolve(selected: &str) -> Result<Option<NetworkContext>, String> {
    let interfaces = enumerate()?;
    select(&interfaces, selected, usable_mac)
}

#[cfg(test)]
mod tests {
    use super::{enumerate, list, normalize_mac, select, usable_mac, RawInterface};
    use std::cell::Cell;

    const GOOD_MAC: &str = "AA:BB:CC:DD:EE:FF";
    const PLACEHOLDER_MAC: &str = "00:00:00:00:00:00";

    fn raw(name: &str, internal: bool, ipv4: Option<&str>, mac: Option<&str>) -> RawInterface {
        RawInterface {
            name: name.into(),
            ipv4: ipv4.map(str::to_string),
            mac: mac.map(str::to_string),
            internal,
        }
    }

    #[test]
    fn accepts_only_colon_separated_mac_addresses() {
        assert_eq!(
            normalize_mac("AA:bb:01:02:03:ff"),
            Some("aa:bb:01:02:03:ff".into())
        );
        assert!(normalize_mac("aabbccddeeff").is_none());
        assert!(normalize_mac("00:00:00:00:00").is_none());
    }

    #[test]
    fn usable_mac_prefers_the_reported_value() {
        // 系统上报的 MAC 有效时不得触发 fallback（macOS 上那是子进程）。
        let interface = raw("en0", false, Some("192.0.2.10"), Some(GOOD_MAC));
        assert_eq!(usable_mac(&interface), Some("aa:bb:cc:dd:ee:ff".into()));
    }

    #[test]
    fn selected_interface_wins_without_parsing_others() {
        let interfaces = vec![
            raw("en0", false, Some("192.0.2.10"), Some(PLACEHOLDER_MAC)),
            raw("en1", false, Some("192.0.2.11"), Some(PLACEHOLDER_MAC)),
        ];
        let calls = Cell::new(0);
        let resolved = select(&interfaces, "en1", |_| {
            calls.set(calls.get() + 1);
            Some("aa:bb:cc:00:00:02".into())
        })
        .unwrap()
        .unwrap();
        assert_eq!(resolved.interface_name, "en1");
        assert_eq!(resolved.ipv4, "192.0.2.11");
        assert_eq!(resolved.mac, "aa:bb:cc:00:00:02");
        assert_eq!(calls.get(), 1, "指定网卡时只该解析这一条的 MAC");
    }

    #[test]
    fn default_selection_takes_first_usable_in_name_order() {
        // 故意乱序传入：旧实现是在名字排序后的列表里取第一个，选择顺序必须与输入顺序无关。
        let interfaces = vec![
            raw("en5", false, Some("192.0.2.15"), None),
            raw("en2", false, Some("192.0.2.12"), None),
            raw("en0", false, Some("192.0.2.10"), None),
        ];
        let calls = Cell::new(0);
        let resolved = select(&interfaces, "  ", |interface| {
            calls.set(calls.get() + 1);
            Some(format!("mac-{}", interface.name))
        })
        .unwrap()
        .unwrap();
        assert_eq!(resolved.interface_name, "en0");
        assert_eq!(resolved.mac, "mac-en0");
        assert_eq!(calls.get(), 1, "命中第一个候选后不应继续解析");
    }

    #[test]
    fn candidate_without_usable_mac_falls_through_to_next() {
        let interfaces = vec![
            raw("en0", false, Some("192.0.2.10"), Some(PLACEHOLDER_MAC)),
            raw("en1", false, Some("192.0.2.11"), Some(PLACEHOLDER_MAC)),
        ];
        let calls = Cell::new(0);
        let resolved = select(&interfaces, "", |interface| {
            calls.set(calls.get() + 1);
            (interface.name == "en1").then(|| "aa:bb:cc:00:00:02".to_string())
        })
        .unwrap()
        .unwrap();
        assert_eq!(resolved.interface_name, "en1");
        assert_eq!(resolved.mac, "aa:bb:cc:00:00:02");
        assert_eq!(calls.get(), 2, "en0 判定失败后必须继续检查 en1");
    }

    #[test]
    fn fallback_mac_can_make_a_candidate_eligible() {
        // 原始 MAC 无效（None）但 fallback 拿到有效值：仍按原规则选中。
        let interfaces = vec![raw("en0", false, Some("192.0.2.10"), None)];
        let resolved = select(&interfaces, "", |_| Some("aa:bb:cc:00:00:01".into()))
            .unwrap()
            .unwrap();
        assert_eq!(resolved.interface_name, "en0");
        assert_eq!(resolved.ipv4, "192.0.2.10");
        assert_eq!(resolved.mac, "aa:bb:cc:00:00:01");
    }

    #[test]
    fn no_usable_interface_reports_the_same_results_as_before() {
        let interfaces = vec![
            raw("lo0", true, Some("127.0.0.1"), Some(GOOD_MAC)),
            raw("utun0", false, None, Some(GOOD_MAC)),
            raw("en9", false, Some("192.0.2.19"), None),
        ];
        let calls = Cell::new(0);
        let mut resolve_mac = |_: &RawInterface| {
            calls.set(calls.get() + 1);
            None
        };
        assert_eq!(select(&interfaces, "", &mut resolve_mac).unwrap(), None);
        assert_eq!(
            calls.get(),
            1,
            "虚拟网卡与没有 IPv4 的网卡不该走到 MAC 解析，只有 en9 需要"
        );
        assert_eq!(
            select(&interfaces, "missing", &mut resolve_mac)
                .unwrap_err()
                .as_str(),
            "找不到网卡：missing"
        );
        assert_eq!(
            select(&interfaces, "utun0", &mut resolve_mac)
                .unwrap_err()
                .as_str(),
            "网卡 utun0 没有 IPv4 地址"
        );
        assert_eq!(
            select(&interfaces, "en9", &mut resolve_mac)
                .unwrap_err()
                .as_str(),
            "网卡 en9 没有可用 MAC 地址"
        );
    }

    #[test]
    fn list_preserves_every_interface_and_name_order() {
        // `list()` 是 GUI 网卡列表的唯一来源：必须保留全部网卡（含无 IPv4、无 MAC 的）
        // 且按名字排序。读不到网卡的环境（受限沙箱）直接跳过——选择逻辑的单元测试
        // 不依赖真实网卡。
        let (Ok(raw), Ok(listed)) = (enumerate(), list()) else {
            return;
        };
        assert_eq!(listed.len(), raw.len(), "GUI 列表不能丢掉任何网卡");
        let names: Vec<&str> = listed.iter().map(|item| item.name.as_str()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "列表必须按网卡名排序");
        for item in &listed {
            let source = raw
                .iter()
                .find(|entry| entry.name == item.name)
                .expect("list() 不能凭空造出网卡");
            assert_eq!(item.ipv4, source.ipv4);
            assert_eq!(item.internal, source.internal);
        }
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
        let fallback = super::fallback_mac("en0");
        assert!(fallback.is_some() || mac.is_none());
    }
}
