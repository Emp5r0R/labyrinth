# Repository Guidelines

## Project Overview
Labyrinth is a Rust 2021 network tunneling and reverse port-forwarding tool with
three CLI modes:
- `server`: TCP/TLS or QUIC/UDP control server with interactive or headless
  operation.
- `agent`: outbound TCP/TLS or QUIC/UDP client that registers host/network
  details and handles server requests.
- `dweller`: persistent inbound listener installed or started on a host and
  authenticated by the server.

Use this guide as the current source of truth for contributor orientation. Keep
user-facing behavior in `README.md` aligned when CLI behavior changes.

## Project Structure & Module Organization
- `src/main.rs` - Clap CLI entry point for `server`, `agent`, and `dweller`.
- `src/cli.rs` - shared Clap argument structs and role dispatch used by every
  binary.
- `src/bin/` - dedicated `labyrinth-server`, `labyrinth-agent`, and
  `labyrinth-dweller` entry points.
- `src/lib.rs` - library exports used by integration tests and benches.
- `src/server/mod.rs` - server startup, transport listener setup, interactive
  CLI, command dispatch, headless mode, and Portal streaming orchestration.
- `src/server/chain_manager.rs` - smart access planning for terminal-first
  multi-hop workflows across agents, active tunnels, and remembered dwellers.
- `src/server/core.rs` - `LabyrinthServer` state: connected agents, selected
  agent, auth, streaming managers, port-forward listeners, Ariadne listeners,
  and dweller registry.
- `src/server/agent_manager.rs` and `src/server/agent_connection.rs` - agent
  registration/authentication and protocol message routing.
- `src/server/tunnel_manager.rs` - Ariadne/TUN setup and teardown.
- `src/server/topology.rs` - agent route inference, route ownership snapshots,
  shared-network detection, and conflict detection for multi-hop planning.
- `src/server/network_map.rs` - read-only terminal map renderer for agents,
  dwellers, detected networks, tunnels, Portal forwards, and shared routes.
- `src/server/dashboard.rs` - browser visualization server and JSON snapshot
  API for the live network map and smart access suggestions.
- `src/server/reverse_port_forward.rs` - Portal reverse port-forwarding support.
- `src/server/dweller_manager.rs` and `src/server/dweller_registry.rs` -
  dweller install, connect, runtime callback/hibernation configuration,
  remembered path metadata, queued task persistence, and `dwellers.json`.
- `src/server/certificate.rs`, `src/server/privileges.rs`, `src/server/ui.rs` -
  certificate handling, privilege checks, and display helpers.
- `src/server/netstack_bridge_windows.rs` - Windows-only netstack bridge.
- `src/agent/core.rs` - agent and dweller runtime loops, low-noise outbound
  reachability reporting, hibernating dweller task polling, callback check-ins,
  and message handling.
- `src/agent/connection.rs` and `src/agent/tls_config.rs` - TCP/TLS,
  QUIC/UDP, SOCKS connection setup, and certificate verification. SOCKS proxy
  mode applies to TCP/TLS only.
- `src/agent/reverse_port_forward.rs` and `src/agent/streaming_manager.rs` -
  agent-side reverse forwarding and stream handling.
- `src/agent/command_executor.rs` and `src/agent/pty_shell.rs` - remote command
  execution helpers and PTY shell session support.
- `src/agent/system_info.rs` - host profile and interface collection.
- `src/streaming/` - streaming traits, models, errors, connection and stream
  managers, metrics, and recovery. `resource_manager.rs` and
  `test_interfaces.rs` are test-only modules.
- Shared modules: `src/config.rs`, `src/error.rs`, `src/protocol.rs`,
  `src/security.rs`, `src/styling.rs`, and `src/transport.rs`.
- Tests: `tests/integration_streaming.rs`.
- Benches: `benches/streaming_benchmarks.rs`.
- Assets: `assets/peas/` fallback PEAS scripts used by command workflows.
- Docs: `README.md` stays concise; `docs/usage.md` holds operator workflows,
  `docs/architecture.md` holds module/design boundaries, `docs/operations.md`
  holds build/test/release and git hygiene, and `docs/reports/` holds older
  validation summaries.
- Releases/tooling: `releases/`, `build_release.sh`, and generated
  `command_outputs/` or `shell_sessions/` artifacts.

## Build, Test, and Development Commands
- Build debug: `cargo build`
- Build release: `cargo build --release`
- Run interactive server:
  `LABYRINTH_AUTH_KEY=<secret> cargo run --bin labyrinth-server -- --listen-addr 0.0.0.0:44344`
- Run headless server:
  `LABYRINTH_AUTH_KEY=<secret> cargo run --bin labyrinth-server -- --headless --listen-addr 0.0.0.0:44344`
- Run QUIC server:
  `LABYRINTH_AUTH_KEY=<secret> cargo run --bin labyrinth-server -- --transport quic --listen-addr 0.0.0.0:44344`
- Local unauthenticated server testing only:
  `cargo run --bin labyrinth-server -- --no-auth`
- Browser visualization is enabled by default on `127.0.0.1:44777`. Move it
  with `--web-ui-addr <ip:port>` or disable it with `--no-web-ui`.
- Run agent:
  `cargo run --bin labyrinth-agent -- --server 127.0.0.1:44344 --fingerprint <sha256> [--retry]`
- Run agent over QUIC:
  `cargo run --bin labyrinth-agent -- --transport quic --server 127.0.0.1:44344 --fingerprint <sha256> [--retry]`
- Run agent through SOCKS5:
  `cargo run --bin labyrinth-agent -- --server 127.0.0.1:44344 --proxy socks5://127.0.0.1:1080 --fingerprint <sha256>`
- Run dweller listener:
  `cargo run --bin labyrinth-dweller -- --listen 0.0.0.0:45454 --cert-file cert.pem --key-file key.pem --id <id> --auth-key <secret>`
- Run compatibility wrapper:
  `cargo run --bin labyrinth -- server|agent|dweller ...`
- Format: `cargo fmt`
- Format check: `cargo fmt -- --check`
- Lint: `cargo clippy -- -D warnings`
- Tests: `cargo test`
- Focused streaming tests: `cargo test --test integration_streaming -- --nocapture`
- Benches: `cargo bench`
- Release binaries: `./build_release.sh` builds the compatibility wrapper plus
  dedicated server, agent, and dweller artifacts.
- Docs sanity: keep `README.md`, `docs/usage.md`, `docs/architecture.md`, and
  `docs/operations.md` aligned with CLI and runtime behavior when features or
  flags change.

There is no current `--enable-streaming` CLI flag; streaming support is enabled
by default in config and used by Portal mode. The server accepts `--interface` and
`--route` for headless compatibility, but verify the implementation before
relying on automatic tunnel startup from those flags.

## Runtime Behavior Notes
- Interactive server commands include `help`, `agents`, `dwellers`, `select`,
  `connect-dweller`, `drop-dweller`, `configure-dweller`, `task-dweller`,
  `dweller-tasks`, `forget-dweller`, `info`, `plan <ip|cidr>`, `access
  `chain status`, `chain doctor [ip|cidr]`, `tunnel` / `ariadne`,
  `topology` / `routes`, `map` / `network-map`, `forward` / `portal`,
  `commands` / `cmd`, `bloodhound`, `upload`, `download`, `status`, `cert`, `stop`, and
  `exit`.
- Smart access is terminal-first. `plan` must be read-only. `access` must show
  the chosen path and require confirmation before mutating tunnels or dweller
  connections. Keep this path idempotent: reuse active tunnels, do not duplicate
  listeners, and make blockers explicit.
- `map` is visualization-only. It reads current server snapshots and should not
  mutate route ownership, active tunnels, dwellers, port forwards, or selected
  agent state.
- The browser dashboard remains read-mostly in this version. Keep
  `GET /api/network-map` typed and derived from the same server snapshots as
  terminal topology/map views. Control endpoints should only be added after the
  terminal smart access workflow is stable and should default to localhost or
  explicit token protection.
- `--transport tcp` is the default server-agent transport. `--transport quic`
  runs the server-agent control stream over QUIC/UDP using the same certificate
  fingerprint trust model. For QUIC-connected agents, Portal and Linux Ariadne
  accepted TCP flows open native per-connection QUIC bidirectional streams
  instead of carrying payloads as JSON stream messages on the control stream.
  Keep transport changes behind `TransportMode` and do not remove the TCP/TLS
  path without a migration plan.
- The Shell category inside `commands` offers a raw SSH/WinRM-style terminal
  and a line-oriented control shell. The raw terminal forwards key presses to
  the remote PTY and uses `Ctrl-]` as the local detach sequence.
- `LABYRINTH_AUTH_KEY` is required by default for server-agent authentication.
  Agents read the same environment variable through collected system info.
- Ariadne auto-detects candidate IPv4 routes from the selected agent's
  `NetworkInterface.addresses`, normalizes host CIDRs to network CIDRs, skips
  loopback/link-local ranges, and uses the best route as the prompt default.
- `cert.pem` and `key.pem` are loaded from the working directory or generated on
  first server start. The `cert` command prints the fingerprint and base64 cert
  for agent verification.
- Ariadne requires elevated privileges for TUN setup. Treat privilege warnings
  as expected when running without root/admin rights.
- Dweller state is persisted in `dwellers.json` in the server working directory.
- Dwellers are persistent remembered listeners for future access. Smart access
  may connect a remembered dweller automatically after a parent tunnel makes its
  listen address reachable, then refresh topology and continue planning.
- Dweller records also store callback server targets and the parent path observed
  during drop, such as `C via B via A`. Treat the stored path as operator
  context; active routing decisions must still be based on live topology and
  active tunnel state.
- Dwellers default to hibernating callback mode with configurable sleep, jitter,
  and task batch size. TCP/TLS and QUIC callbacks use the existing framed control
  protocol. HTTP, HTTPS, and DNS are accepted as explicit callback transport
  labels for configuration/planning, but need dedicated listeners before they can
  carry task traffic.
- Hibernating dweller work is pull-based: the server queues multiple tasks in
  `dwellers.json`, the dweller claims a bounded batch on check-in, executes each
  task, returns results, and sleeps again. Long-lived tunnels and port forwards
  should keep `hibernation=false` so the control channel remains online.
- Agent and dweller registrations include a low-noise outbound reachability
  report. The built-in check uses local route-table inspection and a short TCP
  check to the configured Labyrinth server only.

## Coding Style & Naming Conventions
- Use Rust 2021 and 4-space indentation.
- Module and file names use `snake_case`; types/enums use `PascalCase`;
  constants use `SCREAMING_SNAKE_CASE`.
- Prefer existing async/Tokio patterns, typed protocol messages, and local
  manager APIs over ad hoc cross-module shortcuts.
- Keep network protocol changes in `src/protocol.rs` synchronized with both
  server and agent handlers.
- Add comments only for non-obvious control flow, protocol decisions, or
  platform-specific behavior.

## Testing Guidelines
- Unit tests live near the modules they exercise; integration streaming tests
  live in `tests/integration_streaming.rs`.
- Prefer deterministic async tests with explicit `tokio::time::timeout` bounds.
- Cover both successful paths and failure/cleanup behavior, especially for auth,
  TLS, tunnel teardown, port-forward listener cleanup, and dweller registry
  persistence.
- Some networking or TUN behavior can be environment-sensitive. If a test needs
  elevated privileges or a free local port, make the skip/failure reason clear.
- Run `cargo test` before submitting. Add `cargo bench` for changes that affect
  streaming throughput, latency, or resource management.

## Security & Configuration Tips
- Only use Labyrinth against systems and networks where you have explicit
  authorization.
- Do not use `--no-auth` outside local testing.
- Do not commit real certificates, keys, auth keys, generated dwellers, command
  outputs, or shell session captures.
- Keep generated artifacts ignored: `target/`, `releases/`, `command_outputs/`,
  `shell_sessions/`, `dwellers.json`, local certificates, local keys, logs, and
  generated dweller payload/config output should not be tracked.
- Keep `LABYRINTH_AUTH_KEY` high-entropy in real deployments.
- Use certificate fingerprint or base64 certificate verification for agents.
- Review command execution, upload/download, PTY shell, PEAS, and dweller changes
  with extra care because they affect remote host control.

## Commit & Pull Request Guidelines
- Prefer Conventional Commits, for example:
  `feat(dweller): persist installed listeners`,
  `fix(server): clean up portal listener on disconnect`,
  `test(streaming): cover connection recovery`.
- PRs should include the problem, approach, user-visible behavior changes, and
  exact verification commands.
- Update `README.md` when CLI flags, interactive commands, security defaults, or
  release workflow change.
