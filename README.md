# Labyrinth v1.0.0

```
 )   _ ( _        _ o  _  _)_ ( _
(__ (_( )_) (_(  )  ( ) ) (_   ) )
              _)

                 by Emp5r0R
```

Labyrinth is a Rust network access and visualization toolkit for authorized
security testing. It provides an operator server, outbound agents, persistent
dwellers, IP tunneling, reverse port forwarding, interactive shells, QUIC
streaming, automated BloodHound collection, and read-only network map visualization.

Use it only on systems and networks where you have explicit authorization.

## Current Capabilities

- Server, agent, and dweller modes through dedicated binaries or the
  compatibility wrapper.
- TCP/TLS transport by default, with optional QUIC/UDP transport for native
  per-connection streams.
- Optional SNI and ALPN overrides for TCP/TLS and QUIC agent handshakes.
- Certificate fingerprint or base64 certificate verification for agent and
  dweller trust.
- Ariadne IP tunneling with automatic route candidate detection from connected
  agent interfaces.
- Portal reverse port forwarding for service-specific access.
- Smart multi-hop planning with `plan`, `access`, and `chain doctor`.
- Automated BloodHound (SharpHound) collection with auto-discovery,
  provisioning from GitHub, secure execution, and result retrieval.
- Remembered dwellers with callback server configuration, observed parent path
  metadata, low-noise outbound reachability status, hibernation, jitter, and
  queued task polling.
- Interactive remote shells with raw PTY mode and a control shell that uses
  `!` local commands so `/usr/bin`, `/tmp`, and other slash-prefixed remote
  paths pass through unchanged.
- Windows in-memory execution workflows for BOF and reflective PE/DLL loading.
  Linux in-memory loading requires a separate ELF/memfd loader design and is
  not part of the current BOF/PE workflow.
- Explicit Windows evasion startup hooks for AMSI and ETW through the agent
  `--evasion` option. These hooks are never applied unless requested.
- Optional read-only browser network map on `127.0.0.1:44777` with `--gui`, showing
  agents, dwellers, networks, active tunnels, port forwards, encrypted edges,
  local edges, shared networks, route conflicts, and status summaries.
- Linux and Windows agent and dweller support. Linux Ariadne uses TUN support;
  Windows bridge work is isolated under the Windows netstack module.

## Documentation

- [Usage guide](docs/usage.md): install, quick start, CLI flags, interactive
  commands, dweller behavior, shell behavior, examples, and troubleshooting.
- [Architecture guide](docs/architecture.md): server, agent, dweller,
  streaming, transport, topology, and dashboard boundaries.
- [Operations guide](docs/operations.md): build, test, release, git hygiene,
  generated files, and security handling.
- [Agent instructions](AGENTS.md): contributor rules and module map for coding
  agents.
- [Historical reports](docs/reports/): previous summaries and validation notes.

## Quick Start

Build from source:

```bash
cargo build --release --bins
```

Start the server:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./target/release/labyrinth-server \
  --listen-addr 0.0.0.0:44344
```

The server prints a certificate fingerprint. Use that fingerprint when starting
agents:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./target/release/labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --retry
```

Optional transport and Windows startup controls:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./target/release/labyrinth-agent \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --sni example.com \
  --alpn h2,http/1.1 \
  --evasion amsi,etw
```

`--evasion` is Windows-only and accepts `amsi`, `etw`, or `all`. Hook patching
is implemented for x86_64 and i686 Windows and skips unsupported architectures
with a warning. Use it only inside explicitly authorized test scope.

Open the visualization dashboard on the server host by starting the server with `--gui`:

```text
http://127.0.0.1:44777
```

Useful first commands in the server console:

```text
labyrinth -> agents
labyrinth -> select
labyrinth -> info
labyrinth -> map
labyrinth -> plan 10.10.0.0/24
labyrinth -> access 10.10.0.0/24
labyrinth -> commands
```

## QUIC Transport

QUIC avoids the usual TCP-over-TCP behavior for proxied internal connections by
running the server-agent control channel over UDP and opening a lightweight QUIC
bidirectional stream for each Portal or supported Ariadne flow.

Server:

```bash
sudo LABYRINTH_AUTH_KEY="change-this-secret" \
  ./target/release/labyrinth-server \
  --transport quic \
  --listen-addr 0.0.0.0:44344
```

Agent:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  ./target/release/labyrinth-agent \
  --transport quic \
  --server SERVER_IP:44344 \
  --fingerprint SHA256_FINGERPRINT
```

SOCKS proxy mode applies to TCP/TLS only because QUIC uses UDP.

## Dweller Quick Start

Dwellers are remembered access points for future connectivity. They can listen
for inbound server connections or check in to configured callback servers when
reachable.

```bash
./target/release/labyrinth-dweller \
  --listen 0.0.0.0:45454 \
  --cert-file cert.pem \
  --key-file key.pem \
  --id branch-01 \
  --name branch-host \
  --auth-key "change-this-secret" \
  --callback-server SERVER_IP:44344 \
  --callback-fingerprint SHA256_FINGERPRINT \
  --sleep 60 \
  --jitter 50 \
  --task-batch-size 10
```

Default dweller callback behavior is hibernating task polling:

```text
sleep
check for queued tasks
run claimed tasks
send results
sleep again
```

Use `--hibernation false` for dwellers that must keep long-lived tunnels or
port forwards online.

## Security Defaults

- Set `LABYRINTH_AUTH_KEY` unless you are doing local-only testing.
- Do not use `--no-auth` outside isolated tests.
- Pin server identity with `--fingerprint` or `--cert`.
- Treat `--sni`, `--alpn`, `--evasion`, in-memory execution, BloodHound,
  upload/download, and shell workflows as explicit operator actions that require
  authorization and change-control review.
- Keep generated certificates, keys, logs, command output, shell captures,
  dwellers, and release binaries out of git.
- Keep the browser dashboard bound to localhost unless you add explicit access
  control around it.

## Verification

Before submitting changes:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
git diff --check
```

For streaming-specific changes:

```bash
cargo test --test integration_streaming -- --nocapture
cargo bench
```

## License and Responsibility

This repository does not grant permission to test third-party systems. Operators
are responsible for authorization, scope control, logging requirements, and
local law.
