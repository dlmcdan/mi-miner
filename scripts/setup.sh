#!/bin/bash
set -e

echo "=== mi-miner Setup ==="
echo ""

# Check Rust
if ! command -v cargo &> /dev/null; then
    echo "Rust not found. Installing..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
else
    echo "Rust: $(rustc --version)"
fi

# Check Xcode Metal tools
echo ""
if xcrun -sdk macosx metal --version &> /dev/null; then
    echo "Metal compiler: available (GPU mining supported)"
else
    echo "Metal compiler: not available"
    echo "  GPU mining requires Xcode (not just command line tools)."
    echo "  Install from the App Store for GPU support."
    echo "  CPU mining works without it."
fi

# Build
echo ""
echo "Building..."
cargo build --release
echo ""
echo "Build complete: ./target/release/mi-miner"

# Generate config if missing
if [ ! -f "$HOME/.mi-miner/config.toml" ]; then
    echo ""
    echo "Generating default config..."
    ./target/release/mi-miner --generate-config
fi

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Quick start:"
echo "  ./scripts/run.sh          Start mining (dashboard at http://127.0.0.1:7878)"
echo "  ./scripts/benchmark.sh    Test your hashrate"
echo "  ./scripts/test.sh         Run tests"
echo ""
echo "On first launch, open http://127.0.0.1:7878 to create your wallet."
