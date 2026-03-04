#!/bin/bash

# Labyrinth Multi-Architecture Build Script
# Version 1.0.0 by Emp5r0R

echo "=== Labyrinth v1.0.0 Multi-Architecture Build ==="
echo "Building for multiple Linux architectures..."
echo

# Create release directory
mkdir -p releases

# Define working targets and their descriptions
declare -A TARGETS=(
    ["x86_64-unknown-linux-gnu"]="Linux x86_64 (GNU - dynamic)"
    ["x86_64-unknown-linux-musl"]="Linux x86_64 (musl - static)"
)

# Build for each target
for target in "${!TARGETS[@]}"; do
    echo "Building for ${TARGETS[$target]} ($target)..."
    
    # Build the binary
    if cargo build --release --target "$target"; then
        # Copy binary to releases directory with descriptive name
        binary_name="labyrinth-v1.0.0-$target"
        cp "target/$target/release/labyrinth" "releases/$binary_name"
        
        # Make it executable
        chmod +x "releases/$binary_name"
        
        # Get file size
        size=$(du -h "releases/$binary_name" | cut -f1)
        echo "✓ Built: $binary_name ($size)"
    else
        echo "✗ Failed to build for $target"
    fi
    echo
done

echo "=== Build Summary ==="
echo "Successfully built binaries:"
ls -la releases/

echo
echo "=== Binary Information ==="
for binary in releases/labyrinth-v1.0.0-*; do
    if [ -f "$binary" ]; then
        echo "$(basename "$binary"):"
        echo "  Size: $(du -h "$binary" | cut -f1)"
        echo "  Type: $(file "$binary" | cut -d: -f2-)"
        echo
    fi
done

echo "=== Usage Notes ==="
echo "• labyrinth-v1.0.0-x86_64-unknown-linux-gnu: Standard Linux binary (requires glibc)"
echo "• labyrinth-v1.0.0-x86_64-unknown-linux-musl: Static binary (works on any Linux)"
echo
echo "Build completed successfully!"
echo "by Emp5r0R"