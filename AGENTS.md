# Repository Guidelines

## Project Structure & Module Organization
- `src/main.rs` — CLI entry (`server` and `agent` subcommands).
- `src/server/` — server-side core: `agent_manager.rs`, `tunnel_manager.rs`, `ui.rs`, `certificate.rs`, `privileges.rs`, `streaming_reverse_port_forward.rs`.
- `src/agent/` — agent functionality: `connection.rs`, `core.rs`, `tls_config.rs`, `system_info.rs`, `reverse_port_forward.rs`.
- `src/streaming/` — streaming architecture: `stream_manager.rs`, `connection_manager.rs`, `traits.rs`, `models.rs`, `errors.rs`.
- Shared: `src/config.rs`, `src/error.rs`, `src/protocol.rs`, `src/styling.rs`.
- Tests: `tests/integration_streaming.rs` (Tokio async integration tests).
- Benches: `benches/streaming_benchmarks.rs` (Criterion).
- Releases & tooling: `releases/`, `build_release.sh`.

## Build, Test, and Development Commands
- Build debug: `cargo build`
- Run server: `cargo run -- server [--listen-addr 0.0.0.0:44344] [--headless] [--enable-streaming]`
- Run agent: `cargo run -- agent --server 127.0.0.1:44344 [--fingerprint <sha256>] [--retry]`
- Tests: `cargo test -- --nocapture` (runs Tokio-based integration tests)
- Benches: `cargo bench`
- Release binaries (multi-arch): `./build_release.sh`

## Coding Style & Naming Conventions
- Language: Rust 2021; 4-space indentation.
- Naming: modules/files `snake_case`; types/enums `PascalCase`; constants `SCREAMING_SNAKE_CASE`.
- Formatting: `cargo fmt` (verify in CI/local with `cargo fmt -- --check`).
- Linting: `cargo clippy -- -D warnings` before submitting.

## Testing Guidelines
- Frameworks: Tokio async tests; integration tests live under `tests/` (e.g., `tests/integration_streaming.rs`).
- Conventions: name tests by domain, e.g., `integration_<feature>.rs`; prefer deterministic async with explicit timeouts.
- Scope: cover happy paths and error handling (e.g., connection failures, cleanup); avoid flaky sleeps—use `tokio::time::timeout`.
- Run full suite locally: `cargo test` and `cargo bench` for performance-sensitive changes.

## Commit & Pull Request Guidelines
- Commits: use Conventional Commits (preferred):
  - Examples: `feat(streaming): add bidirectional stream manager`, `fix(server): handle closed connections`, `test(agent): add retry path`.
- PRs must include:
  - Problem statement and approach, linked issues, and test plan (commands/log excerpts).
  - If user-visible behavior changes, update `README.md` and include screenshots or sample CLI sessions.

## Security & Configuration Tips
- Authentication: set `LABYRINTH_AUTH_KEY` for secure deployments; avoid `--no-auth` outside local testing.
- Certificates: use `cert`/fingerprint for verification; do not commit real secrets. Bundled `cert.pem`/`key.pem` are for local testing only.
- Networking: Fullhouse (TUN) requires root; prefer headless flags in non-interactive environments.

