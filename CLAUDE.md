# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

mi-miner — Solo Bitcoin miner in Rust with Metal GPU acceleration. Targets Apple M4 Max (16-core CPU, 40-core GPU, 128 GB unified memory). Runs as a background daemon with adaptive throttling based on user activity, serves a web dashboard on localhost:7878.

## Build & Development

```bash
./scripts/dev.sh             # Stop running instance, build, and start (primary dev workflow)
./scripts/build.sh           # Build only
./scripts/run.sh             # Start (builds if binary missing)
./scripts/stop.sh            # Stop a running instance
./scripts/test.sh            # Run all tests (118 tests across crates)
./scripts/benchmark.sh       # CPU benchmark (--full for complete suite)
./scripts/setup.sh           # First-time setup (installs Rust, checks Xcode, builds)
```

Direct cargo commands:
```bash
cargo build --release        # Build optimized binary
cargo test --workspace       # Run all tests
cargo check                  # Quick type-check
```

**GPU shader compilation** requires full Xcode (not just command line tools). Without Xcode, the binary builds but GPU mining is disabled at runtime. Install Xcode and ensure `xcrun -sdk macosx metal` works.

## Architecture

6-crate Cargo workspace:

- **mi-core** — Config (TOML), errors (thiserror), shared stats (atomics), Bitcoin primitives (SHA-256d, midstate, merkle root, target), BIP39 wallet generation, hardware auto-detection
- **mi-mining** — CPU SHA-256d engine using `sha2` crate (ARM SHA2 HW accel), thread pool with dynamic scaling, midstate optimization, benchmarking
- **mi-gpu** — Metal compute shader GPU mining engine (`shader.metal`), pipeline/dispatcher/manager, compiled via `build.rs`
- **mi-network** — Stratum v1 client (async TCP, JSON-RPC, auto-reconnect), Bitcoin Core RPC client (placeholder)
- **mi-activity** — User idle detection (macOS CGEventSource FFI, Linux evdev), system CPU monitoring (sysinfo), adaptive throttle logic
- **mi-web** — Axum web dashboard with SSE live stats, wallet onboarding (generate or use existing), settings panel, mining controls (start/pause/resume/stop), connection test, benchmark, auto-configure

Key patterns:
- `MiningStats` is `Arc`-shared across all subsystems, uses atomics for lock-free updates
- CPU mining uses OS threads (not tokio) via crossbeam channels — CPU-bound work
- Work flows from stratum callback → sync channel → pool feeder thread → crossbeam channel → workers
- Networking, web, and activity monitoring use tokio async runtime
- GPU uses Metal compute shaders with unified memory (MTLResourceStorageModeShared)
- Nonce space: GPU gets 90%, CPU gets 10%
- If no wallet is configured, the miner starts paused with dashboard accessible for setup
