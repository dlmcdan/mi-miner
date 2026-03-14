#!/bin/bash

BINARY="./target/release/mi-miner"

if [ -f "$BINARY" ]; then
    exec "$BINARY" --stop
else
    # Fallback: kill by PID file or process name
    PID_FILE="$HOME/.mi-miner/mi-miner.pid"
    if [ -f "$PID_FILE" ]; then
        kill "$(cat "$PID_FILE")" 2>/dev/null && echo "mi-miner stopped." || echo "mi-miner is not running."
        rm -f "$PID_FILE"
    else
        pkill -f "mi-miner" 2>/dev/null && echo "mi-miner stopped." || echo "mi-miner is not running."
    fi
fi
