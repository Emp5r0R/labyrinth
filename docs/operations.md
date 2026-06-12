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

Use the full verification set before merging behavioral changes:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test
git diff --check
```

For shell-specific changes:

```bash
cargo test shell_ --lib
```

For streaming and Portal changes:

```bash
cargo test --test integration_streaming -- --nocapture
cargo bench
```

For dashboard changes:

- Start the server with the dashboard enabled via `--gui`.
- Open `http://127.0.0.1:44777`.
- Verify empty state, connected agent state, dweller state, active tunnel state,
  route conflicts, zoom, pan, fit-to-view, node selection, and responsive
  behavior.

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

## Documentation Rules

- Keep `README.md` concise and current.
- Put command and workflow details in `docs/usage.md`.
- Put module and design details in `docs/architecture.md`.
- Put build, test, release, and repository hygiene in this file.
- Move old validation reports into `docs/reports/` instead of leaving them in
  the repository root.
- Avoid emojis and decorative language in docs.
