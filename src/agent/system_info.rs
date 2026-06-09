use crate::protocol::{AgentInfo, AgentKind, NetworkInterface};

/// Single Responsibility: System information gathering
pub struct SystemInfoCollector;

impl SystemInfoCollector {
    pub fn get_system_info() -> AgentInfo {
        Self::build_agent_info(AgentKind::Generic, None, None, None, None)
    }

    pub fn build_agent_info(
        kind: AgentKind,
        stable_id: Option<String>,
        listener_addr: Option<String>,
        listener_port: Option<u16>,
        name_override: Option<String>,
    ) -> AgentInfo {
        let hostname = hostname::get()
            .unwrap_or_else(|_| "unknown".into())
            .to_string_lossy()
            .to_string();

        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();

        // Get network interfaces
        let interfaces = Self::get_network_interfaces();

        AgentInfo {
            name: name_override.unwrap_or_else(|| format!("{}@{}", whoami::username(), hostname)),
            hostname,
            os,
            arch,
            interfaces,
            auth_key: std::env::var("LABYRINTH_AUTH_KEY").ok(),
            kind,
            stable_id,
            listener_addr,
            listener_port,
        }
    }

    fn get_network_interfaces() -> Vec<NetworkInterface> {
        if let Ok(output) = std::process::Command::new("ip")
            .args(["addr", "show"])
            .output()
        {
            let interfaces = Self::parse_ip_addr_output(&String::from_utf8_lossy(&output.stdout));
            if !interfaces.is_empty() {
                return interfaces;
            }
        }

        if cfg!(target_os = "windows") {
            if let Ok(output) = std::process::Command::new("ipconfig")
                .args(["/all"])
                .output()
            {
                let interfaces =
                    Self::parse_windows_ipconfig_output(&String::from_utf8_lossy(&output.stdout));
                if !interfaces.is_empty() {
                    return interfaces;
                }
            }
        }

        if let Ok(output) = std::process::Command::new("ifconfig").args(["-a"]).output() {
            let interfaces = Self::parse_ifconfig_output(&String::from_utf8_lossy(&output.stdout));
            if !interfaces.is_empty() {
                return interfaces;
            }
        }

        vec![NetworkInterface {
            name: "unknown".to_string(),
            addresses: vec!["127.0.0.1/8".to_string()],
            hardware_addr: "00:00:00:00:00:00".to_string(),
            mtu: 1500,
            flags: vec!["UP".to_string()],
        }]
    }

    fn parse_ip_addr_output(output: &str) -> Vec<NetworkInterface> {
        let mut interfaces = Vec::new();
        let mut current_interface: Option<NetworkInterface> = None;

        for line in output.lines() {
            let line = line.trim();

            if let Some((index, rest)) = line.split_once(':') {
                if index.parse::<u32>().is_ok() {
                    if let Some((iface_name, _)) = rest.split_once(':') {
                        let iface_name = iface_name.trim();
                        if let Some(iface) = current_interface.take() {
                            interfaces.push(iface);
                        }

                        current_interface = Some(NetworkInterface {
                            name: iface_name.to_string(),
                            addresses: Vec::new(),
                            hardware_addr: String::new(),
                            mtu: Self::parse_mtu(line),
                            flags: Self::parse_angle_flags(line),
                        });
                    }
                }
            }

            if line.starts_with("inet ") {
                if let Some(ref mut iface) = current_interface {
                    if let Some(addr) = line.split_whitespace().nth(1) {
                        iface.addresses.push(addr.to_string());
                    }
                }
            }

            if line.starts_with("link/ether ") {
                if let Some(ref mut iface) = current_interface {
                    if let Some(mac) = line.split_whitespace().nth(1) {
                        iface.hardware_addr = mac.to_string();
                    }
                }
            }
        }

        if let Some(iface) = current_interface {
            interfaces.push(iface);
        }

        interfaces
    }

    fn parse_windows_ipconfig_output(output: &str) -> Vec<NetworkInterface> {
        let mut interfaces = Vec::new();
        let mut current_interface: Option<NetworkInterface> = None;
        let mut pending_ipv4: Option<String> = None;

        for raw_line in output.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            if line.to_ascii_lowercase().contains("adapter ") && line.ends_with(':') {
                if let Some(iface) = current_interface.take() {
                    if !iface.addresses.is_empty() {
                        interfaces.push(iface);
                    }
                }
                pending_ipv4 = None;
                let name = line
                    .trim_end_matches(':')
                    .split_once("adapter ")
                    .map(|(_, name)| name)
                    .unwrap_or("unknown")
                    .trim()
                    .to_string();
                current_interface = Some(NetworkInterface {
                    name,
                    addresses: Vec::new(),
                    hardware_addr: String::new(),
                    mtu: 1500,
                    flags: vec!["UP".to_string()],
                });
                continue;
            }

            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let key = key.to_ascii_lowercase();
            let value = value.trim();

            if key.contains("physical address") {
                if let Some(ref mut iface) = current_interface {
                    iface.hardware_addr = value.replace('-', ":").to_ascii_lowercase();
                }
            } else if key.contains("ipv4 address") {
                pending_ipv4 = value
                    .split_whitespace()
                    .next()
                    .map(|addr| addr.trim_end_matches("(Preferred)").to_string());
            } else if key.contains("subnet mask") {
                if let (Some(ref mut iface), Some(ipv4)) =
                    (current_interface.as_mut(), pending_ipv4.take())
                {
                    if let Some(prefix) = Self::netmask_to_prefix(value) {
                        iface.addresses.push(format!("{}/{}", ipv4, prefix));
                    }
                }
            }
        }

        if let Some(iface) = current_interface {
            if !iface.addresses.is_empty() {
                interfaces.push(iface);
            }
        }

        interfaces
    }

    fn parse_ifconfig_output(output: &str) -> Vec<NetworkInterface> {
        let mut interfaces = Vec::new();
        let mut current_interface: Option<NetworkInterface> = None;

        for raw_line in output.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            if !raw_line.starts_with(char::is_whitespace) && line.contains(':') {
                if let Some(iface) = current_interface.take() {
                    interfaces.push(iface);
                }
                let name = line
                    .split(':')
                    .next()
                    .unwrap_or("unknown")
                    .trim()
                    .to_string();
                current_interface = Some(NetworkInterface {
                    name,
                    addresses: Vec::new(),
                    hardware_addr: String::new(),
                    mtu: Self::parse_mtu(line),
                    flags: Self::parse_angle_flags(line),
                });
                continue;
            }

            if line.starts_with("inet ") {
                if let Some(ref mut iface) = current_interface {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    let ip = parts.get(1).copied();
                    let netmask = parts
                        .windows(2)
                        .find_map(|pair| (pair[0] == "netmask").then_some(pair[1]));

                    if let (Some(ip), Some(netmask)) = (ip, netmask) {
                        if let Some(prefix) = Self::netmask_to_prefix(netmask) {
                            iface.addresses.push(format!("{}/{}", ip, prefix));
                        }
                    }
                }
            }

            if line.starts_with("ether ") {
                if let Some(ref mut iface) = current_interface {
                    if let Some(mac) = line.split_whitespace().nth(1) {
                        iface.hardware_addr = mac.to_string();
                    }
                }
            }
        }

        if let Some(iface) = current_interface {
            interfaces.push(iface);
        }

        interfaces
    }

    fn parse_mtu(line: &str) -> u32 {
        line.split("mtu")
            .nth(1)
            .and_then(|tail| tail.split_whitespace().next())
            .and_then(|value| value.parse().ok())
            .unwrap_or(1500)
    }

    fn parse_angle_flags(line: &str) -> Vec<String> {
        let flags_start = line.find('<').unwrap_or(0);
        let flags_end = line.find('>').unwrap_or(line.len());
        if flags_start < flags_end {
            line[flags_start + 1..flags_end]
                .split(',')
                .filter(|flag| !flag.is_empty())
                .map(|flag| flag.to_string())
                .collect()
        } else {
            Vec::new()
        }
    }

    fn netmask_to_prefix(mask: &str) -> Option<u8> {
        let mask_value = if let Some(hex) = mask.strip_prefix("0x") {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            u32::from(mask.parse::<std::net::Ipv4Addr>().ok()?)
        };

        let mut seen_zero = false;
        let mut prefix = 0;
        for bit in (0..32).rev() {
            if (mask_value & (1 << bit)) != 0 {
                if seen_zero {
                    return None;
                }
                prefix += 1;
            } else {
                seen_zero = true;
            }
        }
        Some(prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::SystemInfoCollector;

    #[test]
    fn parses_linux_ip_addr_output_with_cidr() {
        let output = "\
2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500
    link/ether 00:11:22:33:44:55 brd ff:ff:ff:ff:ff:ff
    inet 192.168.10.42/24 brd 192.168.10.255 scope global eth0
";
        let interfaces = SystemInfoCollector::parse_ip_addr_output(output);
        assert_eq!(interfaces[0].name, "eth0");
        assert_eq!(interfaces[0].addresses, vec!["192.168.10.42/24"]);
        assert_eq!(interfaces[0].hardware_addr, "00:11:22:33:44:55");
    }

    #[test]
    fn parses_windows_ipconfig_output_with_subnet_mask() {
        let output = "\
Ethernet adapter Ethernet:
   Physical Address. . . . . . . . . : 00-11-22-33-44-55
   IPv4 Address. . . . . . . . . . . : 10.0.5.44(Preferred)
   Subnet Mask . . . . . . . . . . . : 255.255.255.0
";
        let interfaces = SystemInfoCollector::parse_windows_ipconfig_output(output);
        assert_eq!(interfaces[0].name, "Ethernet");
        assert_eq!(interfaces[0].addresses, vec!["10.0.5.44/24"]);
        assert_eq!(interfaces[0].hardware_addr, "00:11:22:33:44:55");
    }

    #[test]
    fn parses_ifconfig_output_with_hex_netmask() {
        let output = "\
en0: flags=8863<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500
    ether 00:11:22:33:44:55
    inet 172.16.7.20 netmask 0xffff0000 broadcast 172.16.255.255
";
        let interfaces = SystemInfoCollector::parse_ifconfig_output(output);
        assert_eq!(interfaces[0].name, "en0");
        assert_eq!(interfaces[0].addresses, vec!["172.16.7.20/16"]);
        assert_eq!(interfaces[0].hardware_addr, "00:11:22:33:44:55");
    }
}
