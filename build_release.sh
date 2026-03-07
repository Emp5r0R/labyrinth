#!/bin/bash

# Labyrinth Multi-Architecture Build Script
# Version 1.0.0 by Emp5r0R

set -u

VERSION="1.0.0"
RELEASE_DIR="releases"

echo "=== Labyrinth v${VERSION} Multi-Architecture Build ==="
echo "Building for Linux and Windows targets..."
echo

mkdir -p "${RELEASE_DIR}"

declare -A TARGETS=(
    ["x86_64-unknown-linux-gnu"]="Linux x86_64 (GNU - dynamic)"
    ["x86_64-unknown-linux-musl"]="Linux x86_64 (musl - static)"
    ["x86_64-pc-windows-gnu"]="Windows x86_64 (GNU)"
    ["i686-pc-windows-gnu"]="Windows x86 (GNU)"
)

successful_targets=()
failed_targets=()

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo is required but was not found in PATH"
    exit 1
fi

if ! command -v rustc >/dev/null 2>&1; then
    echo "rustc is required but was not found in PATH"
    exit 1
fi

HOST_TARGET="$(rustc -vV | awk '/^host: / { print $2 }')"

ensure_target_installed() {
    local target="$1"

    # Host target does not require extra installation.
    if [ "${target}" = "${HOST_TARGET}" ]; then
        return 0
    fi

    # If rustup is unavailable (system Rust install), we cannot auto-install targets.
    # Continue and let cargo build decide; this keeps host builds working.
    if ! command -v rustup >/dev/null 2>&1; then
        echo "  -> rustup not found; cannot auto-install ${target}."
        echo "  -> Attempting build anyway (install target manually if this fails)."
        return 0
    fi

    if rustup target list --installed | grep -q "^${target}$"; then
        return 0
    fi

    echo "  -> Rust target ${target} is not installed. Installing..."
    if rustup target add "${target}"; then
        return 0
    fi

    return 1
}

target_binary_name() {
    local target="$1"
    if [[ "${target}" == *"windows"* ]]; then
        echo "labyrinth.exe"
    else
        echo "labyrinth"
    fi
}

output_name_for_target() {
    local target="$1"
    if [[ "${target}" == *"windows"* ]]; then
        echo "labyrinth-v${VERSION}-${target}.exe"
    else
        echo "labyrinth-v${VERSION}-${target}"
    fi
}

check_windows_toolchain() {
    local target="$1"

    if [[ "${target}" == "x86_64-pc-windows-gnu" ]]; then
        if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
            echo "  -> Missing cross-compiler: x86_64-w64-mingw32-gcc"
            echo "  -> Install with: sudo apt install gcc-mingw-w64-x86-64"
            return 1
        fi
    fi

    if [[ "${target}" == "i686-pc-windows-gnu" ]]; then
        if ! command -v i686-w64-mingw32-gcc >/dev/null 2>&1; then
            echo "  -> Missing cross-compiler: i686-w64-mingw32-gcc"
            echo "  -> Install with: sudo apt install gcc-mingw-w64-i686"
            return 1
        fi
    fi

    return 0
}

for target in "${!TARGETS[@]}"; do
    echo "Building for ${TARGETS[$target]} (${target})..."

    binary_name="$(output_name_for_target "${target}")"
    output_path="${RELEASE_DIR}/${binary_name}"

    rm -f "${output_path}"

    if ! ensure_target_installed "${target}"; then
        echo "✗ Failed to prepare target ${target}"
        failed_targets+=("${target}")
        echo
        continue
    fi

    if [[ "${target}" == *"windows"* ]] && ! check_windows_toolchain "${target}"; then
        echo "✗ Missing required Windows cross-compile toolchain for ${target}"
        failed_targets+=("${target}")
        echo
        continue
    fi

    if cargo build --release --target "${target}"; then
        source_binary="target/${target}/release/$(target_binary_name "${target}")"
        cp "${source_binary}" "${output_path}"
        if [[ "${target}" != *"windows"* ]]; then
            chmod +x "${output_path}"
        fi

        size=$(du -h "${output_path}" | cut -f1)
        echo "✓ Built: ${binary_name} (${size})"
        successful_targets+=("${target}")
    else
        echo "✗ Failed to build for ${target}"
        failed_targets+=("${target}")
    fi
    echo
done

if [ -f "assets/wintun/wintun.dll" ]; then
    cp "assets/wintun/wintun.dll" "${RELEASE_DIR}/wintun.dll"
    echo "✓ Included runtime dependency: wintun.dll"
fi

echo "=== Build Summary ==="
if [ ${#successful_targets[@]} -gt 0 ]; then
    echo "Successfully built binaries:"
    for target in "${successful_targets[@]}"; do
        binary="${RELEASE_DIR}/$(output_name_for_target "${target}")"
        echo "  - $(basename "${binary}")"
    done
else
    echo "No binaries were built successfully."
fi

if [ ${#failed_targets[@]} -gt 0 ]; then
    echo
    echo "Failed targets:"
    for target in "${failed_targets[@]}"; do
        echo "  - ${target}"
    done
fi

echo
echo "=== Binary Information ==="
for target in "${successful_targets[@]}"; do
    binary="${RELEASE_DIR}/$(output_name_for_target "${target}")"
    echo "$(basename "${binary}"):"
    echo "  Size: $(du -h "${binary}" | cut -f1)"
    echo "  Type: $(file "${binary}" | cut -d: -f2-)"
    echo
done

echo "=== Usage Notes ==="
echo "• labyrinth-v${VERSION}-x86_64-unknown-linux-gnu: Standard Linux binary (requires glibc)"
echo "• labyrinth-v${VERSION}-x86_64-unknown-linux-musl: Static binary (works on any Linux)"
echo "• labyrinth-v${VERSION}-x86_64-pc-windows-gnu.exe: Windows x64 executable"
echo "• labyrinth-v${VERSION}-i686-pc-windows-gnu.exe: Windows x86 executable"
echo "• Windows Fullhouse mode requires wintun.dll next to the executable"
echo

if [ ${#failed_targets[@]} -gt 0 ]; then
    echo "Build completed with failures."
    echo "by Emp5r0R"
    exit 1
fi

echo "Build completed successfully!"
echo "by Emp5r0R"
