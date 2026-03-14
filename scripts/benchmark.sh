#!/bin/bash
set -e

BINARY="./target/release/mi-miner"

if [ ! -f "$BINARY" ]; then
    echo "Building first..."
    cargo build --release
    echo ""
fi

if [ "$1" = "--full" ]; then
    echo "Running full benchmark suite..."
    exec "$BINARY" --benchmark --full
else
    echo "Running quick benchmark..."
    echo "Use --full for the complete suite"
    echo ""
    exec "$BINARY" --benchmark
fi
