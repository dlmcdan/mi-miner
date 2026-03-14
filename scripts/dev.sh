#!/bin/bash
set -e

# Stop any running instance
pkill -f "target/release/mi-miner" 2>/dev/null && echo "Stopped running instance." && sleep 1 || true

echo "Building mi-miner..."
cargo build --release

echo ""
echo "Starting mi-miner..."
echo "Dashboard: http://127.0.0.1:7878"
echo "Press Ctrl+C to stop"
echo ""

exec ./target/release/mi-miner "$@"
