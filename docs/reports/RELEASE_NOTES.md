# Labyrinth v1.0.0 Release Notes

**Version:** 1.0.0
**Author:** Emp5r0R
**Release Date:** June 14, 2026
**Release URL:** https://github.com/Emp5r0R/labyrinth/releases/tag/v1.0.0

---

## Overview

Labyrinth v1.0.0 is the initial public binary release. It ships the compatibility
wrapper plus dedicated server, agent, and dweller binaries for Windows and Linux.

## Included Binaries

### Compatibility Wrapper
- `labyrinth-v1.0.0-i686-pc-windows-gnu.exe` (5.4M)
- `labyrinth-v1.0.0-x86_64-pc-windows-gnu.exe` (6.0M)
- `labyrinth-v1.0.0-x86_64-unknown-linux-gnu` (6.5M)
- `labyrinth-v1.0.0-x86_64-unknown-linux-musl` (6.6M)

### Server
- `labyrinth-server-v1.0.0-i686-pc-windows-gnu.exe` (4.3M)
- `labyrinth-server-v1.0.0-x86_64-pc-windows-gnu.exe` (4.9M)
- `labyrinth-server-v1.0.0-x86_64-unknown-linux-gnu` (5.3M)
- `labyrinth-server-v1.0.0-x86_64-unknown-linux-musl` (5.4M)

### Agent
- `labyrinth-agent-v1.0.0-i686-pc-windows-gnu.exe` (3.5M)
- `labyrinth-agent-v1.0.0-x86_64-pc-windows-gnu.exe` (4.0M)
- `labyrinth-agent-v1.0.0-x86_64-unknown-linux-gnu` (4.3M)
- `labyrinth-agent-v1.0.0-x86_64-unknown-linux-musl` (4.4M)

### Dweller
- `labyrinth-dweller-v1.0.0-i686-pc-windows-gnu.exe` (3.7M)
- `labyrinth-dweller-v1.0.0-x86_64-pc-windows-gnu.exe` (4.2M)
- `labyrinth-dweller-v1.0.0-x86_64-unknown-linux-gnu` (4.5M)
- `labyrinth-dweller-v1.0.0-x86_64-unknown-linux-musl` (4.6M)

## Platform Notes

- `x86_64-unknown-linux-gnu`: standard Linux build for glibc-based systems.
- `x86_64-unknown-linux-musl`: static Linux build for broader portability.
- `x86_64-pc-windows-gnu.exe`: Windows x64 executable.
- `i686-pc-windows-gnu.exe`: Windows x86 executable.
- Windows Ariadne mode requires `wintun.dll` next to the executable.

## Verification

Release artifacts were built with:

```bash
./build_release.sh
```

Validation gates for this release:

```bash
cargo fmt -- --check
cargo test
cargo clippy --all-targets -- -D warnings
git diff --check
```

The published release targets commit:

```text
3686f978163a6a823aab22bec27de030000157dc
```
