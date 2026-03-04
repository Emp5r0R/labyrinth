use crate::protocol::{AgentInfo, NetworkInterface};

/// Single Responsibility: System information gathering
pub struct SystemInfoCollector;

impl SystemInfoCollector {
    pub fn get_system_info() -> AgentInfo {
        let hostname = hostname::get()
            .unwrap_or_else(|_| "unknown".into())
            .to_string_lossy()
            .to_string();
        
        let os = std::env::consts::OS.to_string();
        let arch = std::env::consts::ARCH.to_string();
        
        // Get network interfaces
        let interfaces = Self::get_network_interfaces();
        
        AgentInfo {
            name: format!("{}@{}", whoami::username(), hostname),
            hostname,
            os,
            arch,
            interfaces,
            auth_key: std::env::var("LABYRINTH_AUTH_KEY").ok(),
        }
    }

    fn get_network_interfaces() -> Vec<NetworkInterface> {
        let mut interfaces = Vec::new();
        
        // Use a simple approach to get network interfaces
        if let Ok(output) = std::process::Command::new("ip")
            .args(["addr", "show"])
            .output() 
        {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let mut current_interface: Option<NetworkInterface> = None;
            
            for line in output_str.lines() {
                let line = line.trim();
                
                // Parse interface line (e.g., "2: eth0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500")
                if let Some(colon_pos) = line.find(':') {
                    if let Some(second_colon) = line[colon_pos + 1..].find(':') {
                        let iface_name = line[colon_pos + 1..colon_pos + 1 + second_colon].trim();
                        if !iface_name.is_empty() && !iface_name.starts_with(' ') {
                            // Save previous interface
                            if let Some(iface) = current_interface.take() {
                                interfaces.push(iface);
                            }
                            
                            // Extract flags and MTU
                            let flags_start = line.find('<').unwrap_or(0);
                            let flags_end = line.find('>').unwrap_or(line.len());
                            let flags_str = if flags_start < flags_end {
                                &line[flags_start + 1..flags_end]
                            } else {
                                ""
                            };
                            let flags: Vec<String> = flags_str.split(',').map(|s| s.to_string()).collect();
                            
                            let mtu = if let Some(mtu_pos) = line.find("mtu ") {
                                line[mtu_pos + 4..].split_whitespace().next()
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(1500)
                            } else {
                                1500
                            };
                            
                            current_interface = Some(NetworkInterface {
                                name: iface_name.to_string(),
                                addresses: Vec::new(),
                                hardware_addr: String::new(),
                                mtu,
                                flags,
                            });
                        }
                    }
                }
                
                // Parse inet line (e.g., "inet 192.168.1.100/24 brd 192.168.1.255 scope global eth0")
                if line.starts_with("inet ") {
                    if let Some(ref mut iface) = current_interface {
                        if let Some(addr) = line.split_whitespace().nth(1) {
                            iface.addresses.push(addr.to_string());
                        }
                    }
                }
                
                // Parse link/ether line (e.g., "link/ether 00:11:22:33:44:55 brd ff:ff:ff:ff:ff:ff")
                if line.starts_with("link/ether ") {
                    if let Some(ref mut iface) = current_interface {
                        if let Some(mac) = line.split_whitespace().nth(1) {
                            iface.hardware_addr = mac.to_string();
                        }
                    }
                }
            }
            
            // Don't forget the last interface
            if let Some(iface) = current_interface {
                interfaces.push(iface);
            }
        }
        
        // Fallback if ip command fails
        if interfaces.is_empty() {
            interfaces.push(NetworkInterface {
                name: "unknown".to_string(),
                addresses: vec!["127.0.0.1/8".to_string()],
                hardware_addr: "00:00:00:00:00:00".to_string(),
                mtu: 1500,
                flags: vec!["UP".to_string()],
            });
        }
        
        interfaces
    }
}
