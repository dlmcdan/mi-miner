#!/bin/bash
set -e

echo "Building mi-miner..."
cargo build --release

echo ""
echo "Build complete: ./target/release/mi-miner"
ls -lh target/release/mi-miner
