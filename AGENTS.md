# Repository Guidelines

## Project Overview
Labyrinth is a Rust 2021 network tunneling and reverse port-forwarding tool with
three CLI modes:
- `server`: TLS control server with interactive or headless operation.
- `agent`: outbound TLS client that registers host/network details and handles
  server requests.
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
- `src/server/mod.rs` - server startup, TLS listener setup, interactive CLI,
  command dispatch, headless mode, and Room streaming orchestration.
- `src/server/core.rs` - `LabyrinthServer` state: connected agents, selected
  agent, auth, streaming managers, port-forward listeners, Fullhouse listeners,
  and dweller registry.
- `src/server/agent_manager.rs` and `src/server/agent_connection.rs` - agent
  registration/authentication and protocol message routing.
- `src/server/tunnel_manager.rs` - Fullhouse/TUN setup and teardown.
- `src/server/topology.rs` - agent route inference, route ownership snapshots,
  shared-network detection, and conflict detection for multi-hop planning.
- `src/server/network_map.rs` - read-only terminal map renderer for agents,
  dwellers, detected networks, tunnels, Room forwards, and shared routes.
- `src/server/dashboard.rs` - read-only browser visualization server and JSON
  snapshot API for the live network map.
- `src/server/reverse_port_forward.rs` - Room reverse port-forwarding support.
- `src/server/dweller_manager.rs` and `src/server/dweller_registry.rs` -
  dweller install, connect, forget, persistence, and `dwellers.json`.
- `src/server/certificate.rs`, `src/server/privileges.rs`, `src/server/ui.rs` -
  certificate handling, privilege checks, and display helpers.
- `src/server/netstack_bridge_windows.rs` - Windows-only netstack bridge.
- `src/agent/core.rs` - agent and dweller runtime loops plus message handling.
- `src/agent/connection.rs` and `src/agent/tls_config.rs` - TLS/SOCKS
  connection setup and certificate verification.
- `src/agent/reverse_port_forward.rs` and `src/agent/streaming_manager.rs` -
  agent-side reverse forwarding and stream handling.
- `src/agent/command_executor.rs` and `src/agent/pty_shell.rs` - remote command
  execution helpers and PTY shell session support.
- `src/agent/system_info.rs` - host profile and interface collection.
- `src/streaming/` - streaming traits, models, errors, connection and stream
  managers, metrics, and recovery. `resource_manager.rs` and
  `test_interfaces.rs` are test-only modules.
- Shared modules: `src/config.rs`, `src/error.rs`, `src/protocol.rs`,
  `src/security.rs`, and `src/styling.rs`.
- Tests: `tests/integration_streaming.rs`.
- Benches: `benches/streaming_benchmarks.rs`.
- Assets: `assets/peas/` fallback PEAS scripts used by command workflows.
- Releases/tooling: `releases/`, `build_release.sh`, and generated
  `command_outputs/` or `shell_sessions/` artifacts.

## Build, Test, and Development Commands
- Build debug: `cargo build`
- Build release: `cargo build --release`
- Run interactive server:
  `LABYRINTH_AUTH_KEY=<secret> cargo run --bin labyrinth-server -- --listen-addr 0.0.0.0:44344`
- Run headless server:
  `LABYRINTH_AUTH_KEY=<secret> cargo run --bin labyrinth-server -- --headless --listen-addr 0.0.0.0:44344`
- Local unauthenticated server testing only:
  `cargo run --bin labyrinth-server -- --no-auth`
- Browser visualization is enabled by default on `127.0.0.1:44777`. Move it
  with `--web-ui-addr <ip:port>` or disable it with `--no-web-ui`.
- Run agent:
  `cargo run --bin labyrinth-agent -- --server 127.0.0.1:44344 --fingerprint <sha256> [--retry]`
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

There is no current `--enable-streaming` CLI flag; streaming support is enabled
by default in config and used by Room mode. The server accepts `--interface` and
`--route` for headless compatibility, but verify the implementation before
relying on automatic tunnel startup from those flags.

## Runtime Behavior Notes
- Interactive server commands include `help`, `agents`, `dwellers`, `select`,
  `connect-dweller`, `drop-dweller`, `forget-dweller`, `info`, `tunnel` /
  `fullhouse`, `topology` / `routes`, `map` / `network-map`, `forward` /
  `room`, `commands` / `cmd`, `upload`, `download`, `status`, `cert`, `stop`,
  and `exit`.
- `map` is visualization-only. It reads current server snapshots and should not
  mutate route ownership, active tunnels, dwellers, port forwards, or selected
  agent state.
- The browser dashboard is also visualization-only. Keep `GET /api/network-map`
  typed, read-only, and derived from the same server snapshots as terminal
  topology/map views. Do not add command execution or mutation endpoints there.
- The Shell category inside `commands` offers a raw SSH/WinRM-style terminal
  and a line-oriented control shell. The raw terminal forwards key presses to
  the remote PTY and uses `Ctrl-]` as the local detach sequence.
- `LABYRINTH_AUTH_KEY` is required by default for server-agent authentication.
  Agents read the same environment variable through collected system info.
- Fullhouse auto-detects candidate IPv4 routes from the selected agent's
  `NetworkInterface.addresses`, normalizes host CIDRs to network CIDRs, skips
  loopback/link-local ranges, and uses the best route as the prompt default.
- `cert.pem` and `key.pem` are loaded from the working directory or generated on
  first server start. The `cert` command prints the fingerprint and base64 cert
  for agent verification.
- Fullhouse requires elevated privileges for TUN setup. Treat privilege warnings
  as expected when running without root/admin rights.
- Dweller state is persisted in `dwellers.json` in the server working directory.

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
- Keep `LABYRINTH_AUTH_KEY` high-entropy in real deployments.
- Use certificate fingerprint or base64 certificate verification for agents.
- Review command execution, upload/download, PTY shell, PEAS, and dweller changes
  with extra care because they affect remote host control.

## Commit & Pull Request Guidelines
- Prefer Conventional Commits, for example:
  `feat(dweller): persist installed listeners`,
  `fix(server): clean up room listener on disconnect`,
  `test(streaming): cover connection recovery`.
- PRs should include the problem, approach, user-visible behavior changes, and
  exact verification commands.
- Update `README.md` when CLI flags, interactive commands, security defaults, or
  release workflow change.
