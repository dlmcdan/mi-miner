# mi-miner

A solo Bitcoin miner written in Rust with Metal GPU acceleration for Apple Silicon.

mi-miner connects to a solo mining pool (default: [solo.ckpool.org](https://solo.ckpool.org)), hashes block headers using CPU (with ARM SHA2 hardware acceleration) and optionally GPU (via Metal compute shaders), and submits shares. It runs as a background daemon, adaptively scales resource usage based on user activity, and serves a live web dashboard.

Solo mining at consumer hashrates is a lottery. The odds of finding a block are astronomical, but the reward is the full block subsidy (~3.125 BTC) sent directly to your wallet.

## Features

- **CPU Mining** with SHA-256d midstate optimization and ARM SHA2 hardware acceleration via the `sha2` crate
- **GPU Mining** via Metal compute shaders on Apple Silicon (M1/M2/M3/M4)
- **Auto-detection** of hardware capabilities (P-cores, GPU, memory) at startup
- **Adaptive throttling** based on user input activity and system CPU load
- **Web dashboard** at `http://127.0.0.1:7878` with:
  - Live hashrate, share, and activity stats via Server-Sent Events
  - Wallet creation with BIP39 mnemonic backup verification
  - Settings panel with connection test, benchmark, and auto-configure
  - Mining controls (pause / resume / stop)
- **Built-in wallet** generation (BIP39 12-word mnemonic, BIP84 `bc1q` address derivation)
- **Stratum v1** protocol support with auto-reconnect
- **Single binary** (~4 MB) with embedded dashboard assets, no external dependencies at runtime
- **118 unit tests** across all crates

## Quick Start

```bash
# One-time setup (installs Rust if needed, builds, generates config)
./scripts/setup.sh

# Start mining
./scripts/run.sh
```

Open `http://127.0.0.1:7878` in your browser. On first visit, the dashboard walks you through wallet creation.

Or manually:

```bash
cargo build --release
./target/release/mi-miner
```

## Requirements

- **Rust** 1.75+ (installed automatically by `setup.sh` if missing)
- **macOS** (primary target) or **Linux**
- **Xcode** (optional, for GPU mining via Metal shaders). Without full Xcode, the miner runs CPU-only and logs a note about it. Install Xcode from the App Store if you want GPU acceleration.

### Verifying GPU support

```bash
xcrun -sdk macosx metal --version
```

If this command succeeds, GPU mining will be enabled on the next build.

## Architecture

6-crate Cargo workspace:

```
mi-miner/
├── Cargo.toml              # Workspace root + binary
├── src/main.rs             # CLI, daemon, orchestration
├── config.example.toml
├── scripts/                # Build/run/test helper scripts
└── crates/
    ├── mi-core/            # Config, errors, stats, Bitcoin primitives, wallet, hardware detection
    ├── mi-mining/          # CPU SHA-256d engine, thread pool, midstate optimization, benchmarks
    ├── mi-gpu/             # Metal compute shader GPU mining engine
    ├── mi-network/         # Stratum v1 client, Bitcoin Core RPC (placeholder)
    ├── mi-activity/        # User activity detection, system monitoring, adaptive throttling
    └── mi-web/             # Axum web dashboard, SSE, settings API, wallet onboarding
```

### Crate Details

| Crate | Purpose |
|-------|---------|
| `mi-core` | Shared types used across the workspace. TOML config with serde, `MiMinerError` via thiserror, `MiningStats` with atomic counters (lock-free, shared via `Arc`), Bitcoin utilities (SHA-256d, midstate computation, merkle root, coinbase TX construction, target/difficulty), BIP39 wallet generation with BIP84 address derivation, hardware auto-detection. |
| `mi-mining` | CPU mining engine. Uses the `sha2` crate which auto-detects ARM SHA2 crypto extensions on Apple Silicon. Midstate optimization pre-hashes the first 64 bytes of the 80-byte header once, then only processes the remaining 16 bytes per nonce (roughly doubles throughput). OS thread pool (not tokio) with dynamic thread count via crossbeam channels. |
| `mi-gpu` | Metal compute shader (`shader.metal`) implementing SHA-256d with midstate optimization. Each GPU thread tests one nonce. Compiled at build time via `build.rs` calling `xcrun metal/metallib`. Uses unified memory (`MTLResourceStorageModeShared`) for zero-copy buffer sharing. Gracefully falls back when Xcode isn't installed. |
| `mi-network` | Stratum v1 client over async TCP (tokio). Newline-delimited JSON-RPC. Handles `mining.subscribe`, `mining.authorize`, `mining.notify`, `mining.submit`, `mining.set_difficulty`. Auto-reconnect with exponential backoff. Bitcoin Core RPC via `getblocktemplate` is stubbed for future implementation. |
| `mi-activity` | Platform-specific user idle detection: macOS uses `CGEventSourceSecondsSinceLastEventType` via direct FFI (no Accessibility permissions required), Linux uses `evdev`. System CPU monitoring via `sysinfo`. Throttle logic: user active = minimum resources, user idle + system idle = full power, user idle + system busy = half power. Configurable ramp-up (30s default) and ramp-down (5s default). |
| `mi-web` | Axum HTTP server with SSE for real-time stats push (1-second intervals). Embedded HTML dashboard (<15 KB, dark theme, no JS build step). REST API for config read/write, wallet generation, mining controls (pause/resume/stop), connection test, quick benchmark, hardware detection, and auto-configure. Web-based onboarding flow with wallet creation and mnemonic backup verification. |

## Configuration

Config lives at `~/.mi-miner/config.toml`. Generate a default:

```bash
./target/release/mi-miner --generate-config
```

Or configure via the web dashboard Settings tab.

See [`config.example.toml`](config.example.toml) for all options with comments.

### Key Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `mining.threads` | P-core count | Number of CPU mining threads |
| `gpu.enabled` | `true` | Enable Metal GPU mining |
| `gpu.intensity` | `1.0` | GPU load factor (0.0-1.0) |
| `stratum.url` | `stratum+tcp://solo.ckpool.org:3333` | Solo mining pool |
| `stratum.worker` | (your address) | Bitcoin address + worker name |
| `activity.idle_timeout_secs` | `120` | Seconds before ramping up to full power |
| `activity.min_threads` | `1` | CPU threads when user is active |
| `web.bind` | `127.0.0.1:7878` | Dashboard bind address |

## Wallet

mi-miner includes built-in wallet generation. No external wallet software is required to get started.

### Web-based setup (recommended)

Start the miner and open `http://127.0.0.1:7878`. If no wallet exists, the dashboard walks you through creating one with a BIP39 12-word recovery phrase and backup verification.

### CLI-based setup

```bash
./target/release/mi-miner --generate-wallet
./target/release/mi-miner --show-wallet
```

### How it works

- Generates 128 bits of entropy via the OS CSPRNG (`getrandom`)
- Derives a BIP39 12-word mnemonic
- Derives a BIP84 (`m/84'/0'/0'/0/0`) native segwit `bc1q` address
- Saves to `~/.mi-miner/wallet.json` with `chmod 600` permissions
- The mnemonic is BIP39-standard and can be imported into Sparrow Wallet, Electrum, or any compatible wallet for recovery

### Security notes

- The wallet file is stored outside the project directory at `~/.mi-miner/wallet.json`
- File permissions are set to owner-read/write only (mode `600`)
- **Write down your 12 words on paper.** If you lose them and find a block, the BTC is unrecoverable.
- For significant holdings, consider using a hardware wallet (Ledger, Trezor) and setting its address in the config instead

### Using your own address

If you already have a Bitcoin wallet, set your receive address directly in the config:

```toml
[stratum]
worker = "bc1qYOUR_ADDRESS.mi-miner"
```

## CLI Reference

```
mi-miner [OPTIONS]

Options:
  -c, --config <PATH>      Path to config file
  -t, --threads <N>        Override CPU thread count
      --benchmark          Run CPU benchmark (10 seconds)
      --full               Full benchmark suite (with --benchmark)
      --cpu-only           Disable GPU mining
      --gpu-only           Disable CPU mining
      --generate-config    Generate default config file
      --generate-wallet    Generate a new Bitcoin wallet
      --show-wallet        Display wallet address and recovery phrase
      --daemon             Run as background daemon
      --stop               Stop a running daemon
      --install-service    Generate macOS launchd plist
  -h, --help               Print help
```

## Web Dashboard

The dashboard is served at `http://127.0.0.1:7878` (configurable) and requires no external assets or CDN.

### Mining Tab

- Combined, CPU, and GPU hashrate (live via SSE)
- Total hashes, shares submitted/accepted/rejected, blocks found
- CPU thread count and GPU intensity gauge
- User activity status and idle time
- Pause / Resume / Stop controls

### Settings Tab

- Hardware detection (CPU cores, GPU, memory, platform)
- **Auto Configure** button that detects hardware and sets optimal settings
- All config fields editable: mining threads, GPU, stratum, activity throttling, logging
- **Test Connection** verifies stratum pool connectivity
- **Run Quick Benchmark** runs a 3-second single-core hashrate test
- Save configuration (writes to `~/.mi-miner/config.toml`)
- Note: stratum and bind address changes require a restart

## Development

### Build

```bash
cargo build --release
```

### Test

```bash
cargo test --workspace
```

118 tests covering:

- **Bitcoin primitives** SHA-256d, genesis block hash verification, midstate correctness across nonce ranges, merkle tree construction, coinbase TX building, target/difficulty conversion
- **Config** Parsing, serialization, TOML round-trips, partial configs with defaults, file save/load, path validation
- **Stats** Atomic counters, concurrent multi-threaded access, snapshot capture, JSON serialization
- **Error types** Display formatting, variant coverage, `std::io::Error` conversion
- **Mining hasher** Midstate vs simple hash comparison, stop signal handling, nonce range exhaustion, target matching, hash count accuracy
- **Block templates** Header construction, prev_hash preservation, extranonce merkle root changes, merkle branch computation
- **Hashrate formatting** All magnitude ranges (H/s through GH/s)
- **Stratum messages** Notify parsing, subscribe result handling, submit construction, JSON-RPC serialization/deserialization, error responses, notifications
- **Session management** Hex decode, prev_hash group reversal, difficulty-to-target, extranonce handling, template processing, invalid input handling
- **Throttle logic** Active/idle/busy states, idle timeout boundary conditions, min thread clamping, ramp value interpolation at start/mid/end/past/zero/decreasing
- **GPU manager** Creation, intensity set/clamp, mine batch when unavailable
- **Web API types** ConfigData from/apply_to/round-trip, serialization, deserialization, response types
- **Wallet** BIP84 address derivation verified against known test vector (`abandon` mnemonic), mnemonic uniqueness
- **Hardware** Detection, auto-configure, serialization

### Benchmark

```bash
./target/release/mi-miner --benchmark          # Quick (10s)
./target/release/mi-miner --benchmark --full    # Full suite with CPU scaling sweep
```

Results are saved to `~/.mi-miner/benchmarks/` with timestamps.

### Scripts

| Script | Description |
|--------|-------------|
| `./scripts/setup.sh` | First-time setup: installs Rust, checks Xcode, builds, generates config |
| `./scripts/run.sh` | Builds if needed, starts the miner |
| `./scripts/build.sh` | Release build |
| `./scripts/test.sh` | Run all tests |
| `./scripts/benchmark.sh` | Quick benchmark (use `--full` for complete suite) |

### Enabling GPU Mining

GPU mining requires full Xcode (not just Command Line Tools):

1. Install Xcode from the App Store
2. Run `xcode-select --install` if prompted
3. Verify: `xcrun -sdk macosx metal --version`
4. Rebuild: `cargo build --release`

The `build.rs` in `mi-gpu` compiles `shader.metal` into a `.metallib` that gets loaded at runtime. If compilation fails, the build succeeds but GPU mining is disabled with a warning.

### Project Structure

```
src/main.rs                          CLI, daemon, startup orchestration
crates/mi-core/src/
  config.rs                          TOML config with serde defaults
  error.rs                           Unified error type (thiserror)
  stats.rs                           Lock-free atomic stats shared across subsystems
  bitcoin_util.rs                    SHA-256d, midstate, merkle root, target, coinbase TX
  wallet.rs                          BIP39 mnemonic generation, BIP84 address derivation
  hardware.rs                        CPU/GPU/memory detection, auto-configure
crates/mi-mining/src/
  hasher.rs                          SHA-256d inner loop with midstate optimization
  worker.rs                          Single mining thread with pause/stop support
  pool.rs                            OS thread pool with dynamic scaling
  block.rs                           Block header + merkle root construction from templates
  bench.rs                           Benchmark suite with scaling sweep
crates/mi-gpu/src/
  shader.metal                       Metal compute kernel (SHA-256d per thread)
  pipeline.rs                        Metal pipeline + buffer setup
  dispatcher.rs                      GPU work submission + result collection
  manager.rs                         Top-level GPU interface with intensity control
  build.rs                           Compiles .metal -> .metallib at build time
crates/mi-network/src/
  stratum/client.rs                  Async TCP Stratum v1 client with auto-reconnect
  stratum/messages.rs                JSON-RPC message types (subscribe, notify, submit)
  stratum/session.rs                 Session state, template conversion, difficulty handling
  rpc/client.rs                      Bitcoin Core RPC placeholder
crates/mi-activity/src/
  platform/macos.rs                  CGEventSource idle detection via FFI
  platform/linux.rs                  evdev input monitoring
  monitor.rs                         Activity polling loop (1-second interval)
  throttle.rs                        Throttle decision logic with ramp smoothing
crates/mi-web/src/
  server.rs                          Axum router setup
  routes.rs                          REST endpoints (stats, config, wallet, controls, tests)
  sse.rs                             Server-Sent Events stream
  assets/index.html                  Embedded dashboard (dark theme, no build step)
```

## How Mining Works

1. **Connect** to a solo mining pool via Stratum v1
2. **Receive work** the pool sends a block template with the previous block hash, coinbase transaction parts, and merkle branches
3. **Construct header** build the 80-byte block header from the template
4. **Hash** compute SHA-256d(header) for billions of nonce values across CPU threads and GPU
5. **Check** if the hash meets the pool's difficulty target, submit the share
6. **Win** if the hash meets the *network* difficulty target, you found a block and the full reward goes to your wallet

### Midstate Optimization

The 80-byte header is processed by SHA-256 in two 64-byte blocks. Since only the nonce (bytes 76-79) changes between attempts, the SHA-256 state after the first 64 bytes (the "midstate") can be computed once and reused for every nonce. This roughly doubles throughput.

### Nonce Space Partitioning

When both CPU and GPU are active, the 2^32 nonce space is partitioned: the GPU gets 90% (nonces 0 to ~3.86B) and the CPU gets the remaining 10%. On nonce exhaustion, the extranonce is incremented, a new merkle root is computed, and the nonce space resets.

### Expected Hashrate

On Apple M4 Max (12 P-cores, 40-core GPU):

| Engine | Estimated Hashrate |
|--------|--------------------|
| CPU (single core) | ~5 MH/s |
| CPU (12 P-cores) | ~50-60 MH/s |
| GPU (with Xcode) | ~5-15 GH/s |
| Network total | ~700 EH/s |

The odds of finding a block with ~10 GH/s are approximately 1 in 70 trillion per 10-minute block interval.

## Data Storage

All persistent data lives in `~/.mi-miner/` (outside the project directory):

| File | Purpose |
|------|---------|
| `config.toml` | Configuration |
| `wallet.json` | BIP39 mnemonic + derived address (mode 600) |
| `mi-miner.pid` | PID file for daemon mode |
| `benchmarks/` | Saved benchmark results |

None of these files are tracked by git.

## Contributing

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Run tests (`cargo test --workspace`)
4. Commit your changes
5. Push to your branch and open a pull request

## License

AGPL-3.0 — see [LICENSE](LICENSE) for details.
