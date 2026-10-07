# Operations Guide

This guide covers local development, verification, release output, and git
hygiene for Labyrinth.

## Development Commands

Build:

```bash
cargo build
cargo build --release --bins
```

Run an interactive server:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-server -- \
  --listen-addr 0.0.0.0:44344
```

Run a headless server:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-server -- \
  --headless \
  --listen-addr 0.0.0.0:44344
```

Run a QUIC server:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-server -- \
  --transport quic \
  --listen-addr 0.0.0.0:44344
```

Run an agent:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-agent -- \
  --server 127.0.0.1:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --retry
```

Run an agent with transport customization:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-agent -- \
  --server 127.0.0.1:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --sni example.com \
  --alpn h2,http/1.1
```

Run a Windows agent with explicit startup hooks:

```bash
LABYRINTH_AUTH_KEY="change-this-secret" \
  cargo run --bin labyrinth-agent -- \
  --server 127.0.0.1:44344 \
  --fingerprint SHA256_FINGERPRINT \
  --evasion amsi,etw
```

Run a dweller:

```bash
cargo run --bin labyrinth-dweller -- \
  --listen 0.0.0.0:45454 \
  --cert-file cert.pem \
  --key-file key.pem \
  --id local-dweller \
  --auth-key "change-this-secret"
```

Run the compatibility wrapper:

```bash
cargo run --bin labyrinth -- server
cargo run --bin labyrinth -- agent --server 127.0.0.1:44344 --fingerprint SHA256_FINGERPRINT
cargo run --bin labyrinth -- dweller --id local-dweller --auth-key "change-this-secret" --cert-file cert.pem --key-file key.pem
```

## Verification

Use repository script before merging behavioral changes. It uses `Cargo.lock`
for every dependency-resolving Cargo command:

```bash
# Library and dedicated-binary unit tests
./scripts/test-all.sh unit

# Integration tests (tests/*.rs only), including streaming/Portal coverage
./scripts/test-all.sh integration

# Every networking unit module plus the real-socket end-to-end suites
./scripts/test-all.sh network

# Real TCP/TLS, QUIC and SOCKS5 end-to-end suite only
./scripts/test-all.sh e2e

# fmt check + unit tests
./scripts/test-all.sh quick

# Full gate: format, unit, integration, Clippy, release binaries, docs, hygiene
./scripts/test-all.sh all
```

`all` is required for production-readiness sign-off. `unit`, `integration`,
`network`, `e2e`, and `quick` are fast feedback modes; `hygiene` runs format,
Clippy, release build, docs, and tracked-file checks without rerunning tests.
`bench` runs Criterion benchmarks and `list` prints every test name. Script
requires Bash 4+.

Options apply to every test mode:

```bash
./scripts/test-all.sh e2e --filter socks5 --nocapture   # narrow and show output
./scripts/test-all.sh network --repeat 25 --keep-going  # flake hunting
```

`--keep-going` runs all steps and prints a pass/fail summary with timings
instead of stopping at the first failure.

Networking tests bind only to `127.0.0.1` ephemeral ports and need no
privileges. Ariadne TUN setup is not exercised end to end because it requires
root/admin; its validation and cleanup logic is unit tested. Tests that need a
refused connection hold a bound-but-not-listening socket instead of reusing a
released port, so parallel runs cannot race.

For shell-specific changes, run focused tests in addition to the relevant
script mode:

```bash
cargo test --locked shell_ --lib
```

For streaming and Portal changes:

```bash
cargo test --locked --test integration_streaming -- --nocapture
cargo bench --locked
```

GitHub Actions runs `./scripts/test-all.sh all` on Ubuntu for pushes and pull
requests targeting `main` or `master`. A Windows job separately compiles all
binaries and runs unit tests, integration tests, and Clippy with warnings
denied. Both jobs use pinned Rust `1.93.1`, lockfile mode, least-privilege
read-only repository access, and cancel superseded runs.

For dashboard changes:

- Start the server with the dashboard enabled via `--gui`.
- Open `http://127.0.0.1:44777`.
- Verify empty state, connected agent state, dweller state, active tunnel state,
  route conflicts, platform execution capability badges, zoom, pan,
  fit-to-view, node selection, and responsive behavior.

## Release Build

The release script builds dedicated and wrapper artifacts:

```bash
./build_release.sh
```

Generated files go under `releases/` and are ignored by git. If an official
release artifact must be published, attach it to the release system rather than
committing it to the source tree.

## Git Hygiene

The repository should track source, tests, docs, lockfiles, and curated assets.
It should not track local runtime state or generated output.

Ignored generated paths include:

- `target/`
- `releases/`
- `command_outputs/`
- `shell_sessions/`
- `server.log`
- `*.log`
- `cert.pem`
- `key.pem`
- `cert_b64.txt`
- `dwellers.json`
- generated dweller binaries or config output
- local `.env` files

Check repository state:

```bash
git status --short
git ls-files -i --exclude-standard
```

If a generated file was accidentally tracked, remove it from the index without
deleting the local copy:

```bash
git rm --cached path/to/generated-file
```

## Security Handling

- Do not commit auth keys, operator credentials, generated certificates,
  private keys, shell logs, command output, or dweller runtime state.
- Keep `LABYRINTH_AUTH_KEY` high entropy outside local tests.
- Prefer certificate fingerprint verification for all agents and dwellers.
- Keep `--no-auth` local and temporary.
- Keep the browser dashboard on localhost unless authentication is added.
- Review command execution, upload/download, shell, PEAS, dweller, and task
  queue changes with extra care.
- Review `--sni`, `--alpn`, `--evasion`, BloodHound, BOF, reflective loading,
  and Linux memfd ELF execution changes with extra care because they affect
  transport identity, telemetry interaction, and in-memory execution behavior.

## Documentation Rules

- Keep `README.md` concise and current.
- Put command and workflow details in `docs/usage.md`.
- Put module and design details in `docs/architecture.md`.
- Put build, test, release, and repository hygiene in this file.
- Move old validation reports into `docs/reports/` instead of leaving them in
  the repository root.
- Avoid emojis and decorative language in docs.
