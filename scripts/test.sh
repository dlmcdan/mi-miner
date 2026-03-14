#!/bin/bash
set -e

echo "Running all tests..."
cargo test --workspace

echo ""
echo "All tests passed."
