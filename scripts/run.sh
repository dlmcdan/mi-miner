#!/bin/bash
set -e

BINARY="./target/release/mi-miner"

# Build if binary doesn't exist
if [ ! -f "$BINARY" ]; then
    echo "Binary not found. Building..."
    cargo build --release
    echo ""
fi

echo "Starting mi-miner..."
echo "Dashboard: http://127.0.0.1:7878"
echo "Press Ctrl+C to stop"
echo ""

exec "$BINARY" "$@"
