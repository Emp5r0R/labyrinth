#!/usr/bin/env bash
# Labyrinth test runner. Run with --help for modes and options.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Library test modules that make up the networking surface. Each entry is a
# libtest filter, so `framing::` matches every test in src/framing.rs.
NETWORK_UNIT_FILTERS=(
    framing::
    portal::
    protocol::
    security::
    transport::
    agent::connection::
    agent::core::
    agent::reverse_port_forward::
    agent::streaming_manager::
    agent::system_info::
    server::agent_connection::
    server::agent_manager::
    server::certificate::
    server::chain_manager::
    server::dweller_manager::
    server::dweller_registry::
    server::quic_stream_bridge::
    server::reverse_port_forward::
    server::topology::
    server::tunnel_manager::
    streaming::
)
NETWORK_INTEGRATION_TESTS=(network_e2e integration_streaming)

usage() {
    cat <<'EOF'
Usage: scripts/test-all.sh [MODE] [OPTIONS]

Modes:
  all          Full production gate: fmt, unit, integration, clippy, release
               build, docs, tracked-file hygiene (default; used by CI)
  unit         Library and binary unit tests
  integration  Integration test targets (tests/*.rs)
  network      Every networking unit module + network_e2e + integration_streaming
  e2e          Real-socket end-to-end suite only (tests/network_e2e.rs)
  quick        fmt check + unit tests; fastest useful pre-commit loop
  hygiene      fmt, clippy, release build, docs, tracked-file checks (no tests)
  bench        Criterion benchmarks
  list         List every test name without running anything

Options:
  -f, --filter PATTERN  Only run tests whose name contains PATTERN
  -n, --nocapture       Show test stdout/stderr
  -r, --repeat N        Repeat the test steps N times (flake hunting)
  -k, --keep-going      Run every step even after a failure; report at end
  -h, --help            Show this help

Examples:
  scripts/test-all.sh network
  scripts/test-all.sh e2e --filter socks5 --nocapture
  scripts/test-all.sh network --repeat 20 --keep-going
EOF
}

mode="all"
filter=""
nocapture=0
repeat=1
keep_going=0

if [[ $# -gt 0 && "$1" != -* ]]; then
    mode="$1"
    shift
fi
while [[ $# -gt 0 ]]; do
    case "$1" in
        -f | --filter)
            [[ $# -ge 2 ]] || { echo "--filter needs a pattern" >&2; exit 2; }
            filter="$2"
            shift 2
            ;;
        -n | --nocapture) nocapture=1; shift ;;
        -r | --repeat)
            [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || {
                echo "--repeat needs a positive integer" >&2
                exit 2
            }
            repeat="$2"
            shift 2
            ;;
        -k | --keep-going) keep_going=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

command -v cargo >/dev/null 2>&1 || {
    echo "cargo not found on PATH" >&2
    exit 127
}

if [[ -t 1 ]]; then
    bold=$'\033[1m' green=$'\033[32m' red=$'\033[31m' dim=$'\033[2m' reset=$'\033[0m'
else
    bold="" green="" red="" dim="" reset=""
fi

declare -a results=()
failures=0

# Arguments placed after `--` for libtest.
libtest_args() {
    local -a args=()
    if [[ -n "$filter" ]]; then
        args+=("$filter")
    fi
    if [[ $nocapture -eq 1 ]]; then
        args+=(--nocapture)
    fi
    printf '%s\n' "${args[@]+"${args[@]}"}"
}

run_step() {
    local name="$1"
    shift
    printf '\n%s==> %s%s\n%s$ %s%s\n' "$bold" "$name" "$reset" "$dim" "$*" "$reset"
    local start=$SECONDS status=0
    "$@" || status=$?
    local elapsed=$((SECONDS - start))
    if [[ $status -eq 0 ]]; then
        results+=("${green}PASS${reset}  ${name} (${elapsed}s)")
    else
        results+=("${red}FAIL${reset}  ${name} (${elapsed}s, exit ${status})")
        failures=$((failures + 1))
        if [[ $keep_going -eq 0 ]]; then
            summary
            exit "$status"
        fi
    fi
}

summary() {
    printf '\n%sSummary%s\n' "$bold" "$reset"
    local line
    for line in "${results[@]+"${results[@]}"}"; do
        printf '  %s\n' "$line"
    done
}

cargo_test() {
    local -a extra=()
    mapfile -t extra < <(libtest_args)
    cargo test --locked --workspace --all-features "$@" -- "${extra[@]+"${extra[@]}"}"
}

run_unit_tests() {
    run_step "unit tests" cargo_test --lib --bins
}

run_integration_tests() {
    # `--tests` would also rerun lib/bin unit tests; `--test '*'` is tests/*.rs only.
    run_step "integration tests" cargo_test --test '*'
}

run_network_tests() {
    if [[ -n "$filter" ]]; then
        # An explicit filter replaces the curated module list.
        run_step "network unit tests (filtered)" cargo_test --lib
    else
        local -a extra=()
        if [[ $nocapture -eq 1 ]]; then
            extra+=(--nocapture)
        fi
        run_step "network unit tests" \
            cargo test --locked --workspace --all-features --lib -- \
            "${NETWORK_UNIT_FILTERS[@]}" "${extra[@]+"${extra[@]}"}"
    fi
    local target
    for target in "${NETWORK_INTEGRATION_TESTS[@]}"; do
        run_step "integration: ${target}" cargo_test --test "$target"
    done
}

run_e2e_tests() {
    run_step "integration: network_e2e" cargo_test --test network_e2e
}

run_fmt_check() {
    run_step "rustfmt" cargo fmt -- --check
}

check_tracked_ignored() {
    local tracked_ignored
    tracked_ignored="$(git ls-files -ci --exclude-standard)"
    if [[ -n "$tracked_ignored" ]]; then
        printf 'Tracked files match ignore rules:\n%s\n' "$tracked_ignored" >&2
        return 1
    fi
}

run_hygiene_checks() {
    run_step "clippy" cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    run_step "release build" cargo build --locked --workspace --release --all-features --bins
    run_step "docs" cargo doc --locked --workspace --all-features --no-deps
    run_step "whitespace" git diff --check
    run_step "tracked ignored files" check_tracked_ignored
}

repeat_tests() {
    local iteration
    for ((iteration = 1; iteration <= repeat; iteration++)); do
        if [[ $repeat -gt 1 ]]; then
            printf '\n%s--- iteration %d/%d ---%s\n' "$bold" "$iteration" "$repeat" "$reset"
        fi
        "$@"
    done
}

case "$mode" in
    unit) repeat_tests run_unit_tests ;;
    integration) repeat_tests run_integration_tests ;;
    network) repeat_tests run_network_tests ;;
    e2e) repeat_tests run_e2e_tests ;;
    quick)
        run_fmt_check
        repeat_tests run_unit_tests
        ;;
    hygiene)
        run_fmt_check
        run_hygiene_checks
        ;;
    bench) run_step "benchmarks" cargo bench --locked --workspace ;;
    list)
        cargo test --locked --workspace --all-features --all-targets -- --list --format terse
        exit 0
        ;;
    all)
        run_fmt_check
        repeat_tests run_unit_tests
        repeat_tests run_integration_tests
        run_hygiene_checks
        ;;
    *)
        printf 'Unknown mode: %s\n\n' "$mode" >&2
        usage >&2
        exit 2
        ;;
esac

summary
if [[ $failures -gt 0 ]]; then
    printf '\n%s%d step(s) failed%s\n' "$red" "$failures" "$reset"
    exit 1
fi
printf '\n%sAll steps passed%s\n' "$green" "$reset"
