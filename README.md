# Labyrinth v1.0.0

```
 )   _ ( _        _ o  _  _)_ ( _  
(__ (_( )_) (_(  )  ( ) ) (_   ) ) 
              _)

                 by Emp5r0R
```

> **Navigate the network maze with precision**

A powerful network tunneling tool that lets you access remote networks securely. Think of it as your digital tunnel through firewalls and network barriers.

## 🚀 Quick Start

### 1. Download Labyrinth

Choose the right version for your system:
- **`labyrinth-server-v1.0.0-x86_64-unknown-linux-gnu`** - Server for most Linux systems
- **`labyrinth-agent-v1.0.0-x86_64-unknown-linux-gnu`** - Outbound agent for most Linux systems
- **`labyrinth-dweller-v1.0.0-x86_64-unknown-linux-gnu`** - Persistent dweller listener for most Linux systems
- **`*-x86_64-unknown-linux-musl`** - Static Linux builds
- **`labyrinth-v1.0.0-*`** - Compatibility wrapper with `server`, `agent`, and `dweller` subcommands

```bash
# Make Linux binaries executable
chmod +x labyrinth-server-v1.0.0-x86_64-unknown-linux-musl
chmod +x labyrinth-agent-v1.0.0-x86_64-unknown-linux-musl
chmod +x labyrinth-dweller-v1.0.0-x86_64-unknown-linux-musl
```

### 2. Start the Server (Your Control Center)

```bash
# Start the server (you need root permissions)
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
    ./labyrinth-server-v1.0.0-x86_64-unknown-linux-musl
```

You'll see:
```
 )   _ ( _        _ o  _  _)_ ( _  
(__ (_( )_) (_(  )  ( ) ) (_   ) ) 
              _)

                 by Emp5r0R

[+] Server started on 0.0.0.0:44344

[+] Labyrinth Control Interface
Navigate the network maze with precision

labyrinth → 
```

> [!IMPORTANT]
> **Get the fingerprint!** Use the `cert` command to see the certificate fingerprint you'll need for secure agent connections.

> [!WARNING]
> **Running without sudo?** You'll see a warning about limited functionality. Fullhouse mode requires root privileges for TUN interface creation.

### 3. Connect an Agent (On Target Machine)

```bash
# Connect to your server
LABYRINTH_AUTH_KEY="change-this-secret" \
./labyrinth-agent-v1.0.0-x86_64-unknown-linux-musl \
    --server YOUR_SERVER_IP:44344 \
    --fingerprint a1b2c3d4e5f6789abcdef...
```

### 4. Start Tunneling

Back on your server, you'll see the agent connected. Now you can:

```bash
labyrinth → agents          # See connected machines
labyrinth → select          # Choose which machine to work with
labyrinth → fullhouse       # Start Fullhouse Mode (IP Tunneling)
labyrinth → room            # Start Room Mode (Port Forwarding)
```

Fullhouse automatically detects likely target CIDRs from the selected agent's
interface addresses. The `agents`, `info`, and `status` commands show the best
auto route, and the `fullhouse` prompt uses it as the default. Type a different
CIDR if you need to pivot through another client network.

## 📖 How to Use Every Feature

### 🖥️ Server Commands

#### Start Server (Basic)
```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-server
```
**What it does:** Starts your control center on port 44344

#### Start Server (Custom Port)
```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-server --listen-addr 0.0.0.0:8080
```
**What it does:** Starts server on port 8080 instead

#### Start Server (No Security - Testing Only)
```bash
sudo ./labyrinth-server --no-auth
```
**What it does:** Disables password protection (dangerous!)

> [!WARNING]
> Only use `--no-auth` for testing. Never in real scenarios!

#### Start Server (Headless Mode)
```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-server --headless
```
**What it does:** Runs without interactive interface (for scripts)

#### Start Server (Advanced Options)
```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-server \
    --interface labyrinth \
    --route 192.168.1.0/24 \
    --domain example.com
```
**What it does:** 
- `--interface`: Set custom TUN interface name for tunneling (headless mode only)
- `--route`: Pre-configure target subnet to route (headless mode only)
- `--domain`: Set custom domain for TLS certificate

---

### 🤖 Agent Commands

#### Basic Connection
```bash
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent --server 192.168.1.100:44344
```
**What it does:** Connects to server at IP 192.168.1.100

#### Secure Connection (Recommended)
```bash
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent \
    --server 192.168.1.100:44344 \
    --fingerprint a1b2c3d4e5f6789abcdef...
```
**What it does:** Connects with certificate verification for security

#### Connection with Auto-Retry
```bash
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent \
    --server 192.168.1.100:44344 \
    --fingerprint a1b2c3d4e5f6789abcdef... \
    --retry
```
**What it does:** Automatically reconnects if connection drops

#### Connection Through Proxy
```bash
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent \
    --server 192.168.1.100:44344 \
    --proxy socks5://127.0.0.1:1080 \
    --fingerprint a1b2c3d4e5f6789abcdef...
```
**What it does:** Connects through a SOCKS5 proxy

#### Connection with Custom Certificate
```bash
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent \
    --server 192.168.1.100:44344 \
    --cert "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t..."
```
**What it does:** Connects using a base64-encoded certificate for verification

---

### 🎮 Interactive Server Commands

Once your server is running, you can use these commands:

#### `help` - Show All Commands
```bash
labyrinth → help
```
**What it shows:** List of all available commands

#### `agents` - List Connected Machines
```bash
labyrinth → agents
```
**What you'll see:**
```
Connected Agents
─────────────────
Agent 1
  ID:      abc12345
  Name:    target-host
  System:  Linux/x86_64
  Status:  Online
  Fullhouse (Tunnel): Inactive
  ───────────────────────
Agent 2
  ID:      def67890
  Name:    web-server
  System:  Windows/amd64
  Status:  Online
  Fullhouse (Tunnel): Active (192.168.1.0/24)
```

#### `select` - Choose a Machine to Work With
```bash
labyrinth → select
```
**What it does:** Shows menu to pick which connected machine to control

#### `info` - Show Machine Details
```bash
labyrinth → info
```
**What you'll see:**
```
Agent Profile
────────────
ID:        abc12345
Name:      target-host
Host:      ubuntu-server
System:    Linux/x86_64
Connection: Connected

Network Interfaces
─────────────────
[1]: eth0 (00:11:22:33:44:55)
    192.168.1.50
    fe80::211:22ff:fe33:4455

[2]: wlan0 (aa:bb:cc:dd:ee:ff)
    10.0.0.100
```

#### `tunnel` or `fullhouse` - Start Fullhouse Mode (IP Tunneling)
```bash
labyrinth → tunnel
# or
labyrinth → fullhouse
```
**What it asks:**
- Target subnet (auto-detected from the selected agent when possible)
- Interface name (like `labyrinth`)

**What it does:** Creates a tunnel so you can access the entire network

#### `forward` or `room` - Start Room Mode (Port Forwarding)
```bash
labyrinth → forward
# or
labyrinth → room
```
**What it asks:**
- Port mappings (like `8080:192.168.1.50:80`)
- Type `done` when finished

**What it does:** Forwards specific ports bidirectionally between your machine and the target

#### `status` - Show Current Status
```bash
labyrinth → status
```
**What you'll see:**
```
Labyrinth Status
───────────────
Server:              Running
Security:            Authentication Enabled
Agents:              2
Active connections:  1

Selected Agent
──────────────
Agent:               target-host (abc12345)
Fullhouse (Tunnel):  Active - 192.168.1.0/24
System:              Linux/x86_64
```

#### `commands` or `cmd` - Execute System Commands
```bash
labyrinth → commands
# or
labyrinth → cmd
```
**What it does:** Execute system commands on the selected agent based on its operating system

**What you'll see:**
- **Linux systems**: Options to run `ifconfig`, `ss -tunlp`
- **Windows systems**: Options to run `ipconfig`, `netstat -aon`
- **Unknown systems**: No commands available message

**Example output:**
```
Commands Mode
─────────────
[+] Linux system identified

Available commands:
  1. ifconfig
  2. ss -tunlp
  3. Back

Select a command to execute
> 1

[+] Executing command: ifconfig
[+] Command sent to agent. Waiting for response...

[+] Command Output:
  eth0: flags=4163<UP,BROADCAST,RUNNING,MULTICAST>  mtu 1500
        inet 192.168.1.100  netmask 255.255.255.0  broadcast 192.168.1.255
        ether 00:11:22:33:44:55  txqueuelen 1000  (Ethernet)
```

**Features:**
- **30-second timeout**: Commands automatically timeout if they take too long
- **Real-time feedback**: See command execution status and results immediately
- **Error handling**: Clear error messages if commands fail
- **Formatted output**: Clean, readable command results with proper indentation

#### `stop` - Stop Current Tunnel
```bash
labyrinth → stop
```
**What it does:** Stops the active tunnel or port forwarding

#### `cert` - Show Certificate Info
```bash
labyrinth → cert
```
**What you'll see:**
```
Server Certificate Information
─────────────────────────────

Fingerprint (SHA-256)
  Readable:     a1:b2:c3:d4:e5:f6:78:9a:bc:de:f0:12:34:56:78:9a:bc:de:f0:12:34:56:78:9a:bc:de:f0:12:34:56:78:9a
  Copy-friendly: a1b2c3d4e5f6789abcdef012345678abcdef012345678abcdef012345678abcdef0123456789a

Certificate (Base64)
  LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t...
```

> [!TIP]
> **Easy Fingerprint Copying**: Use the "Copy-friendly" format (without colons) for agent connections!

#### `clear` - Clear Screen
```bash
labyrinth → clear
```
**What it does:** Clears the screen

#### `exit` - Quit Server
```bash
labyrinth → exit
```
**What it does:** Shuts down the server

---

---

## 💡 Real-World Examples

### Example 1: Access Office Network from Home

**Scenario:** You want to access your office network (192.168.1.0/24) from home.

**Step 1:** Set up server at home
```bash
sudo ./labyrinth server
# Note the fingerprint: a1b2c3d4e5f6...
```

**Step 2:** Run agent on office computer
```bash
./labyrinth agent \
    --server YOUR_HOME_IP:44344 \
    --fingerprint a1b2c3d4e5f6...
```

**Step 3:** Create tunnel
```bash
labyrinth → agents          # See office computer
labyrinth → select          # Choose it
labyrinth → fullhouse       # Start tunnel
# Enter: 192.168.1.0/24
# Enter: office
```

**Step 4:** Access office network
```bash
# Now you can access office machines
ping 192.168.1.10
ssh user@192.168.1.20
```

### Example 2: Access Web Server Behind Firewall

**Scenario:** Access a web server at 192.168.1.50:80 through a compromised machine.

**Step 1:** Start server
```bash
sudo ./labyrinth server
```

**Step 2:** Connect agent
```bash
./labyrinth agent --server YOUR_IP:44344 --fingerprint a1b2c3d4e5f6...
```

**Step 3:** Set up port forwarding
```bash
labyrinth → room
# Enter: 8080:192.168.1.50:80
# Enter: done
```

**Step 4:** Access the web server
```bash
# Open browser to http://localhost:8080
# It will show the web server at 192.168.1.50:80
```

### Example 3: Secure Connection with Password

**Step 1:** Set password on server
```bash
export LABYRINTH_AUTH_KEY="my-super-secret-password-2024"
sudo ./labyrinth server
```

**Step 2:** Connect with same password
```bash
export LABYRINTH_AUTH_KEY="my-super-secret-password-2024"
./labyrinth agent \
    --server SERVER_IP:44344 \
    --fingerprint a1b2c3d4e5f6...
```

---

## ⚠️ Security Tips

> [!TIP]
> **Always use fingerprint verification** in real scenarios:
> ```bash
> --fingerprint a1b2c3d4e5f6789abcdef...
> ```

> [!WARNING]
> **Never use these in production:**
> - `--no-auth` (no password protection)

> [!NOTE]
> **Use strong passwords:**
> ```bash
> export LABYRINTH_AUTH_KEY="VeryLongAndComplexPassword2024!"
> ```

---

## 🐛 Troubleshooting

### "Permission denied" when starting server
```bash
# You need root permissions for network tunneling
sudo ./labyrinth server
```

### "Connection refused"
```bash
# Check if server is running
netstat -tlnp | grep 44344

# Try with retry flag
./labyrinth agent --retry --server IP:44344
```

### "Certificate verification failed"
```bash
# Use the exact fingerprint from server startup
./labyrinth agent --fingerprint EXACT_FINGERPRINT_HERE
```

### Agent keeps disconnecting
```bash
# Use auto-retry
LABYRINTH_AUTH_KEY="change-this-secret" ./labyrinth-agent --retry --server IP:44344
```

---

## 📋 Command Reference

### Server Options
| Option | Description | Example |
|--------|-------------|---------|
| `--listen-addr` | Change server port | `--listen-addr 0.0.0.0:8080` |
| `--no-auth` | Disable password | `--no-auth` |
| `--headless` | No interactive mode | `--headless` |
| `--interface` | Custom TUN interface name | `--interface labyrinth` |
| `--route` | Pre-configure target subnet | `--route 192.168.1.0/24` |
| `--domain` | Custom domain for TLS cert | `--domain example.com` |

### Agent Options
| Option | Description | Example |
|--------|-------------|---------|
| `--server` | Server to connect to | `--server 192.168.1.100:44344` |
| `--fingerprint` | Verify certificate | `--fingerprint a1b2c3...` |
| `--cert` | Use base64 certificate | `--cert "LS0tLS1..."` |
| `--retry` | Auto-reconnect | `--retry` |
| `--proxy` | Use proxy | `--proxy socks5://127.0.0.1:1080` |

### Dweller Options
| Option | Description | Example |
|--------|-------------|---------|
| `--listen` | Inbound listen address | `--listen 0.0.0.0:45454` |
| `--cert-file` | TLS certificate PEM path | `--cert-file cert.pem` |
| `--key-file` | TLS key PEM path | `--key-file key.pem` |
| `--id` | Stable dweller identifier | `--id branch-01` |
| `--name` | Optional display name | `--name branch-host` |
| `--auth-key` | Server/dweller shared secret | `--auth-key <secret>` |

### Interactive Commands
| Command | What it does |
|---------|--------------|
| `help` | Show all commands |
| `agents` | List connected machines |
| `dwellers` | List remembered dwellers |
| `select` | Choose machine to control |
| `connect-dweller` | Connect to a remembered dweller |
| `drop-dweller` | Install a persistent dweller through the selected agent |
| `forget-dweller` | Remove a remembered dweller |
| `info` | Show machine details |
| `topology` or `routes` | Show route ownership, shared networks, and conflicts |
| `tunnel` or `fullhouse` | Start Fullhouse mode (IP tunneling) |
| `forward` or `room` | Start Room mode (Port forwarding) |
| `commands` or `cmd` | Execute system commands on agent |
| `upload` | Upload file to selected agent |
| `download` | Download file from selected agent |
| `status` | Show current status |
| `stop` | Stop active tunnel/forwarding |
| `cert` | Show certificate |
| `exit` | Quit server |

---

## 🎯 What Makes Labyrinth Special

- **Easy to Use**: Simple commands, clear feedback
- **Secure by Default**: TLS encryption, certificate verification
- **Two Modes**: Interactive for exploration, direct commands for automation
- **Cross-Platform**: Works on any Linux system
- **No Dependencies**: Static binary needs nothing else
- **Professional**: Clean interface, reliable operation
- **SOLID Architecture**: Modular, maintainable, and extensible codebase
- **Smart UX**: Copy-friendly fingerprints, sudo warnings, clear error messages

## 🏗️ Architecture & Design

Labyrinth follows **SOLID principles** for clean, maintainable code:

### 📁 Project Structure
```
labyrinth/src/
├── main.rs                # Compatibility wrapper CLI
├── cli.rs                 # Shared CLI args and role dispatch
├── bin/
│   ├── labyrinth-server.rs
│   ├── labyrinth-agent.rs
│   └── labyrinth-dweller.rs
├── agent/                    # Agent-side components
│   ├── command_executor.rs  # OS-aware command workflows
│   ├── connection.rs        # Connection management
│   ├── core.rs             # Agent and dweller runtime loops
│   ├── pty_shell.rs        # Interactive PTY shell sessions
│   ├── reverse_port_forward.rs # Agent-side port forwarding
│   ├── streaming_manager.rs # Agent-side streaming support
│   ├── system_info.rs      # System information collection
│   └── tls_config.rs       # TLS configuration
├── server/                  # Server-side components
│   ├── agent_connection.rs # Protocol routing for connected agents
│   ├── agent_manager.rs    # Agent registration & management
│   ├── certificate.rs      # Certificate management
│   ├── core.rs            # Server state management
│   ├── dweller_manager.rs # Dweller install/connect workflows
│   ├── dweller_registry.rs # Persisted dweller registry
│   ├── netstack_bridge_windows.rs # Windows Fullhouse bridge
│   ├── privileges.rs      # Sudo privilege detection
│   ├── reverse_port_forward.rs # Room port forwarding
│   ├── tunnel_manager.rs  # Tunnel operations
│   └── ui.rs              # User interface operations
├── streaming/              # Streaming architecture
│   ├── connection_manager.rs # Connection lifecycle management
│   ├── errors.rs          # Streaming error types
│   ├── metrics.rs         # Metrics and health checks
│   ├── models.rs          # Data structures
│   ├── recovery.rs        # Error recovery coordinator
│   ├── resource_manager.rs # Test-only resource management
│   ├── stream_manager.rs   # Bidirectional stream handling
│   ├── test_interfaces.rs # Test-only stream interfaces
│   └── traits.rs          # Core streaming interfaces
├── config.rs               # Configuration structures
├── error.rs               # Error types and handling
├── protocol.rs            # Network protocol definitions
├── security.rs            # TLS/fingerprint helpers
└── styling.rs             # UI styling and formatting
```

### 🎯 SOLID Principles Implementation

- **Single Responsibility**: Each module has one clear purpose
- **Open/Closed**: Extensible without modification
- **Liskov Substitution**: Proper trait implementations
- **Interface Segregation**: Clean, focused interfaces
- **Dependency Inversion**: Abstractions over concretions

### 🔧 UX Improvements

- **Copy-Friendly Fingerprints**: No more manual colon removal!
- **Sudo Warnings**: Clear notifications about privilege requirements
- **Enhanced Error Messages**: Actionable guidance for common issues
- **Command Organization**: Primary (server/agent) vs secondary commands
- **Smart Agent Management**: Automatic disconnection detection with grace periods for new connections
- **Topology Awareness**: Route ownership and shared-network detection for multi-hop planning
- **Improved Port Forwarding**: Enhanced reliability and proper traffic routing
- **OS-Aware Commands**: Automatic detection of Linux/Windows systems with appropriate command sets
- **Clean Terminology**: "Fullhouse" for tunneling, "Room" for port forwarding

---

## 📞 Need Help?

- **Found a bug?** Open an issue on GitHub
- **Need a feature?** Create a feature request
- **Security concern?** Contact Emp5r0R directly

---

**Made with ❤️ by Emp5r0R**

*Navigate the network maze with precision*
 
---

## 🌐 Fullhouse Netstack (Server-Only)

Labyrinth’s Fullhouse mode is designed for L3 access without requiring root on the agent. To complete TCP handshakes and route arbitrary flows originating from the TUN, the server can run a small userland TCP/IP stack and bridge payloads to the agent over the streaming channel.

- Implementation: `src/server/netstack_bridge_windows.rs` on Windows; Linux
  Fullhouse remains in `src/server/tunnel_manager.rs`.
- Build with smoltcp backend:
  - Build: `cargo build --features netstack_smoltcp --bins`
  - Run wrapper: `cargo run --features netstack_smoltcp --bin labyrinth -- server`
  - Run dedicated server: `cargo run --features netstack_smoltcp --bin labyrinth-server`
- Behavior:
  - Server owns the TUN and userland stack.
  - New TCP flows from the TUN trigger `Stream.Setup {connection_id, mapping}` to the agent.
  - Data flows via `Stream.Data` (TargetToClient / ClientToTarget).
  - On close, `Stream.Close` cleans up both sides.
