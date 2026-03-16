use clap::Parser;
use mi_core::config::MinerConfig;
use mi_core::MiningStats;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Parser)]
#[command(name = "mi-miner", about = "Solo Bitcoin miner with Metal GPU acceleration")]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Run in daemon mode (background)
    #[arg(long)]
    daemon: bool,

    /// Run benchmark mode
    #[arg(long)]
    benchmark: bool,

    /// Full benchmark suite
    #[arg(long)]
    full: bool,

    /// Generate default config file
    #[arg(long)]
    generate_config: bool,

    /// Stop a running daemon
    #[arg(long)]
    stop: bool,

    /// CPU-only mining (no GPU)
    #[arg(long)]
    cpu_only: bool,

    /// GPU-only mining (no CPU)
    #[arg(long)]
    gpu_only: bool,

    /// Install as launchd service (macOS)
    #[arg(long)]
    install_service: bool,

    /// Override number of mining threads
    #[arg(short, long)]
    threads: Option<usize>,

    /// Generate a new Bitcoin wallet (BIP39 mnemonic + bc1q address)
    #[arg(long)]
    generate_wallet: bool,

    /// Show existing wallet address and mnemonic
    #[arg(long)]
    show_wallet: bool,
}

fn main() {
    let cli = Cli::parse();

    if cli.generate_config {
        let path = MinerConfig::default_path();
        let config = MinerConfig::default();
        match config.save(&path) {
            Ok(()) => {
                println!("Config written to {}", path.display());
                println!("Edit the file to set your Bitcoin address in [stratum].worker");
            }
            Err(e) => {
                eprintln!("Error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    if cli.stop {
        stop_daemon();
        return;
    }

    if cli.generate_wallet {
        generate_wallet();
        return;
    }

    if cli.show_wallet {
        show_wallet();
        return;
    }

    let config_path = cli.config.unwrap_or_else(MinerConfig::default_path);
    let mut config = if config_path.exists() {
        match MinerConfig::load(&config_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error loading config: {e}");
                eprintln!("Run `mi-miner --generate-config` to create a default config");
                std::process::exit(1);
            }
        }
    } else {
        eprintln!(
            "No config found at {}. Using defaults.",
            config_path.display()
        );
        eprintln!("Run `mi-miner --generate-config` to create a config file.\n");
        MinerConfig::default()
    };

    // CLI overrides
    if cli.cpu_only {
        config.gpu.enabled = false;
        config.mining.cpu_only = true;
    }
    if cli.gpu_only {
        config.mining.gpu_only = true;
    }
    if let Some(threads) = cli.threads {
        config.mining.threads = threads;
    }

    // Auto-detect GPU availability if not explicitly overridden
    if !cli.cpu_only && !cli.gpu_only && config.gpu.enabled {
        let hw = mi_core::hardware::detect();
        if !hw.gpu_available {
            config.gpu.enabled = false;
            config.mining.cpu_only = true;
            eprintln!("GPU: not available (Metal shader not compiled — install Xcode for GPU mining)");
        } else if let Some(ref name) = hw.gpu_name {
            eprintln!("GPU: {name}");
        }
        eprintln!("CPU: {} P-cores / {} total", hw.cpu_cores_performance, hw.cpu_cores_total);
    }

    // Always sync worker with wallet address (wallet is the source of truth)
    let needs_wallet = if let Some(address) = mi_core::wallet::get_wallet_address() {
        let expected_worker = format!("{address}.mi-miner");
        if config.stratum.worker != expected_worker {
            eprintln!("Syncing worker with wallet address: {address}");
            config.stratum.worker = expected_worker;
        }
        false
    } else if config.stratum.worker.starts_with("YOUR_BITCOIN_ADDRESS") {
        true
    } else {
        false // Manual address set, no wallet file — keep it
    };

    if cli.benchmark {
        mi_mining::run_benchmark(10, config.mining.threads, cli.full);
        return;
    }

    if cli.daemon {
        daemonize();
    }

    if cli.install_service {
        install_launchd_service();
        return;
    }

    init_logging(&config);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    rt.block_on(run(config, needs_wallet));
}

async fn run(config: MinerConfig, needs_wallet: bool) {
    tracing::info!("mi-miner v0.1.0 starting");
    tracing::info!("CPU threads: {}", config.mining.threads);
    tracing::info!("GPU enabled: {}", config.gpu.enabled);

    let stats = MiningStats::new();
    let prior_persistent = stats.load_persistent();
    let prior_uptime = prior_persistent.total_uptime_secs;
    let live_config = mi_core::LiveConfig::new(config.clone());
    let (block_tx, _) = tokio::sync::broadcast::channel::<u64>(16);

    let pid_path = mi_core::config::dirs_path().join("mi-miner.pid");
    let _ = std::fs::create_dir_all(mi_core::config::dirs_path());
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    // Signal handler — first Ctrl+C triggers graceful shutdown, second forces exit
    let stats_signal = stats.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutdown signal received (Ctrl+C again to force quit)");
        stats_signal.should_stop.store(true, Ordering::Relaxed);

        // Wait for a second Ctrl+C — if it comes, force exit immediately
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("Forced shutdown");
        std::process::exit(1);
    });

    // GPU manager
    let gpu_available = if config.gpu.enabled && !config.mining.cpu_only {
        let mgr = mi_gpu::GpuManager::new(stats.clone(), config.gpu.batch_size_log2);
        if mgr.is_available() {
            tracing::info!("GPU mining: ACTIVE");
            true
        } else {
            tracing::warn!("GPU mining: NOT AVAILABLE (falling back to CPU-only)");
            false
        }
    } else {
        tracing::info!("GPU mining: DISABLED");
        false
    };

    // Channel for share submission to stratum
    let (submit_tx, submit_rx) = tokio::sync::mpsc::channel(64);

    // Separate SharedWork for CPU and GPU — each gets a different extranonce2,
    // producing different headers so they search independent hash spaces.
    let cpu_shared_work = mi_mining::worker::SharedWork::new();
    let gpu_shared_work = mi_mining::worker::SharedWork::new();

    // CPU mining pool — workers hash continuously, polling SharedWork for new jobs
    let submit_tx_mining = submit_tx.clone();
    let mining_pool = if !config.mining.gpu_only {
        let pool = mi_mining::MiningPool::with_shared_work(
            config.mining.threads,
            stats.clone(),
            Box::new(move |work, nonce, _hash| {
                tracing::info!(nonce = nonce, "CPU found valid nonce!");
                let submission = mi_network::stratum::client::ShareSubmission {
                    job_id: work.job_id.clone(),
                    extranonce2: work.extranonce,
                    ntime: work.ntime.clone(),
                    nonce,
                };
                let _ = submit_tx_mining.try_send(submission);
            }),
            cpu_shared_work.clone(),
        );
        Some(pool)
    } else {
        None
    };

    // Work distribution: stratum callback builds separate headers for CPU and GPU
    // CPU gets extranonce2=0, GPU gets extranonce2=1 — different coinbase → different
    // merkle root → different header bytes → completely independent hash spaces.
    let on_work: Arc<mi_network::stratum::client::WorkCallback> = {
        let cpu_work = cpu_shared_work.clone();
        let gpu_work = gpu_shared_work.clone();
        Arc::new(Box::new(
            move |template: mi_mining::block::BlockTemplate, target: [u8; 32]| {
                let ntime = format!("{:08x}", template.timestamp);

                // CPU work: extranonce2 = 0
                let (_hdr_cpu, header_cpu) = template.build_header(0);
                cpu_work.update(mi_mining::worker::Work {
                    header: header_cpu,
                    target,
                    job_id: template.job_id.clone(),
                    extranonce: 0,
                    ntime: ntime.clone(),
                });

                // GPU work: extranonce2 = 1 (different merkle root → independent hash space)
                let (_hdr_gpu, header_gpu) = template.build_header(1);
                gpu_work.update(mi_mining::worker::Work {
                    header: header_gpu,
                    target,
                    job_id: template.job_id.clone(),
                    extranonce: 1,
                    ntime,
                });

                tracing::info!(job = template.job_id, "New work → CPU (en2=0) + GPU (en2=1)");
            },
        ))
    };

    // GPU mining thread — reads its own SharedWork and dispatches Metal compute batches
    if gpu_available {
        let shared_work = gpu_shared_work.clone();
        let stats_gpu = stats.clone();
        let submit_tx_gpu = submit_tx.clone();
        let batch_size_log2 = config.gpu.batch_size_log2;

        std::thread::Builder::new()
            .name("gpu-miner".to_string())
            .spawn(move || {
                let mut mgr = mi_gpu::GpuManager::new(stats_gpu.clone(), batch_size_log2);
                let mut last_gen: u64 = 0;
                let mut gpu_nonce: u32 = 0;

                tracing::info!("GPU mining thread started");

                loop {
                    if stats_gpu.should_stop.load(Ordering::Relaxed) {
                        break;
                    }

                    while stats_gpu.paused.load(Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        if stats_gpu.should_stop.load(Ordering::Relaxed) {
                            return;
                        }
                    }

                    // Check for new work
                    if let Some((gen, _work)) = shared_work.get_if_new(last_gen) {
                        last_gen = gen;
                        gpu_nonce = 0;
                    }

                    // Update GPU intensity from throttle (checked every batch)
                    let throttle_intensity =
                        stats_gpu.throttle_gpu_intensity_pct.load(Ordering::Relaxed) as f32
                            / 100.0;
                    mgr.set_intensity(throttle_intensity);

                    // Get current work
                    let work = {
                        let guard = shared_work.work.lock().unwrap();
                        guard.clone()
                    };

                    let work = match work {
                        Some(w) => w,
                        None => {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            continue;
                        }
                    };

                    // Convert header to midstate format for GPU
                    let header = &work.header;
                    let mut midstate = [0u32; 8];
                    let mut tail = [0u32; 4];
                    let mut target = [0u32; 8];

                    // Parse midstate from header bytes 0-31 (first 8 u32 big-endian)
                    // Actually the GPU needs the SHA-256 midstate, not raw header bytes.
                    // For now, pass the header tail words and let the GPU do full hashing.
                    // Tail words must be big-endian u32 to match SHA-256's message schedule.
                    // SHA-256 reads bytes as BE words: w[i] = u32::from_be_bytes(block[i*4..])
                    for i in 0..4 {
                        tail[i] = u32::from_be_bytes([
                            header[64 + i * 4],
                            header[64 + i * 4 + 1],
                            header[64 + i * 4 + 2],
                            header[64 + i * 4 + 3],
                        ]);
                    }

                    // Compute midstate using the same function as CPU
                    let midstate_bytes = mi_core::bitcoin_util::compute_midstate(
                        header.try_into().unwrap(),
                    );
                    for i in 0..8 {
                        midstate[i] = u32::from_be_bytes([
                            midstate_bytes[i * 4],
                            midstate_bytes[i * 4 + 1],
                            midstate_bytes[i * 4 + 2],
                            midstate_bytes[i * 4 + 3],
                        ]);
                    }

                    for i in 0..8 {
                        target[i] = u32::from_be_bytes([
                            work.target[i * 4],
                            work.target[i * 4 + 1],
                            work.target[i * 4 + 2],
                            work.target[i * 4 + 3],
                        ]);
                    }

                    let batch_start = std::time::Instant::now();
                    let (found, _batch_count) = mgr.mine_batch(
                        &midstate, &tail, &target, gpu_nonce,
                    );
                    let batch_elapsed = batch_start.elapsed();

                    gpu_nonce = gpu_nonce.wrapping_add(mgr.batch_size() as u32);

                    if let Some((nonce, _hash)) = found {
                        tracing::info!(nonce = nonce, "GPU found valid nonce!");
                        let submission = mi_network::stratum::client::ShareSubmission {
                            job_id: work.job_id.clone(),
                            extranonce2: work.extranonce,
                            ntime: work.ntime.clone(),
                            nonce,
                        };
                        let _ = submit_tx_gpu.try_send(submission);
                    }

                    // Duty-cycle throttle: sleep proportionally to batch time
                    // At intensity 0.1, run for T then sleep for 9T
                    if throttle_intensity < 0.99 && throttle_intensity > 0.0 {
                        let sleep_ratio = (1.0 / throttle_intensity) - 1.0;
                        let sleep_dur = batch_elapsed.mul_f32(sleep_ratio);
                        // Cap at 500ms to stay responsive to new work/stop signals
                        let sleep_dur = sleep_dur.min(std::time::Duration::from_millis(500));
                        std::thread::sleep(sleep_dur);
                    }

                    // Check for new work between batches
                    if shared_work.generation.load(Ordering::Acquire) != last_gen {
                        continue;
                    }
                }

                tracing::info!("GPU mining thread exiting");
            })
            .ok();
    }

    // Stratum client — only connect if we have a valid wallet/address
    if needs_wallet {
        tracing::warn!("No Bitcoin wallet configured — mining is paused");
        tracing::info!("Open http://{} to create a wallet and start mining", config.web.bind);
        eprintln!();
        eprintln!("  No wallet configured. Open the dashboard to get started:");
        eprintln!();
        eprintln!("    http://{}", config.web.bind);
        eprintln!();
        eprintln!("  Create a wallet in the browser, then restart the miner.");
        eprintln!();
        stats.paused.store(true, Ordering::Relaxed);
    } else if !config.stratum.url.is_empty() {
        let mut client = mi_network::StratumClient::new(
            &config.stratum.url,
            &config.stratum.worker,
            &config.stratum.password,
            stats.clone(),
        );

        // Block-found notification: broadcast to dashboard SSE + macOS notification
        let block_tx_stratum = block_tx.clone();
        client.set_on_block_found(std::sync::Arc::new(move |block_num| {
            let _ = block_tx_stratum.send(block_num);
            fire_macos_notification(block_num);
        }));

        tokio::spawn(async move {
            if let Err(e) = client.run(on_work, submit_rx).await {
                tracing::error!("Stratum client error: {e}");
            }
        });
    }

    // Sleep inhibitor: prevent idle sleep while mining is active
    let mut sleep_inhibitor = mi_activity::SleepInhibitor::new();
    if !needs_wallet && !stats.paused.load(Ordering::Relaxed) {
        sleep_inhibitor.enable();
    }
    let stats_sleep = stats.clone();
    tokio::spawn(async move {
        let mut was_active = !stats_sleep.paused.load(Ordering::Relaxed);
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            interval.tick().await;
            if stats_sleep.should_stop.load(Ordering::Relaxed) {
                sleep_inhibitor.disable();
                break;
            }
            let is_active = !stats_sleep.paused.load(Ordering::Relaxed);
            if is_active && !was_active {
                sleep_inhibitor.enable();
            } else if !is_active && was_active {
                sleep_inhibitor.disable();
            }
            was_active = is_active;
        }
    });

    // Activity monitor
    if config.activity.enabled {
        let (throttle_tx, mut throttle_rx) =
            tokio::sync::watch::channel(mi_activity::throttle::ThrottleState {
                target_threads: config.mining.threads,
                target_gpu_intensity: config.gpu.intensity,
                is_ramping: false,
            });

        let monitor = mi_activity::ActivityMonitor::new(
            live_config.clone(),
            stats.clone(),
            config.mining.threads,
        );

        tokio::spawn(async move {
            if let Err(e) = monitor.run(throttle_tx).await {
                tracing::error!("Activity monitor error: {e}");
            }
        });

        let stats_throttle = stats.clone();
        tokio::spawn(async move {
            while throttle_rx.changed().await.is_ok() {
                let state = throttle_rx.borrow().clone();
                tracing::debug!(
                    threads = state.target_threads,
                    gpu_intensity = state.target_gpu_intensity,
                    "Throttle update"
                );

                // Apply CPU thread throttle
                stats_throttle
                    .active_cpu_threads
                    .store(state.target_threads as u64, Ordering::Relaxed);
                stats_throttle
                    .cpu_threads
                    .store(state.target_threads as u64, Ordering::Relaxed);

                // Apply GPU intensity throttle
                stats_throttle.throttle_gpu_intensity_pct.store(
                    (state.target_gpu_intensity * 100.0) as u64,
                    Ordering::Relaxed,
                );
            }
        });
    }

    // Web dashboard
    if config.web.enabled {
        let bind = config.web.bind.clone();
        let stats_web = stats.clone();
        let lc_web = live_config.clone();
        let block_tx_web = block_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = mi_web::start_server(&bind, stats_web, lc_web, block_tx_web).await {
                tracing::error!("Web server error: {e}");
            }
        });
    }

    // Periodic hasher self-validation (every 60s, costs 1 SHA-256d)
    let stats_validate = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if stats_validate.should_stop.load(Ordering::Relaxed) {
                break;
            }
            match mi_mining::hasher::validate_hasher() {
                Ok(()) => tracing::debug!("Hasher self-check passed"),
                Err(e) => {
                    tracing::error!("CRITICAL: Hasher self-check FAILED: {e}");
                    tracing::error!("Pausing mining — hashes may be incorrect");
                    stats_validate.paused.store(true, Ordering::Relaxed);
                }
            }
        }
    });

    // Hashrate calculator (1-second rolling window)
    let stats_hr = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        // Initialize to current values so the first tick (which fires immediately)
        // doesn't report all historical hashes as a 1-second rate
        let mut last_cpu = stats_hr.cpu_hashes.load(Ordering::Relaxed);
        let mut last_gpu = stats_hr.gpu_hashes.load(Ordering::Relaxed);

        loop {
            interval.tick().await;
            if stats_hr.should_stop.load(Ordering::Relaxed) {
                break;
            }

            let cpu_now = stats_hr.cpu_hashes.load(Ordering::Relaxed);
            let gpu_now = stats_hr.gpu_hashes.load(Ordering::Relaxed);

            stats_hr
                .cpu_hashrate
                .store(cpu_now.saturating_sub(last_cpu), Ordering::Relaxed);
            stats_hr
                .gpu_hashrate
                .store(gpu_now.saturating_sub(last_gpu), Ordering::Relaxed);

            last_cpu = cpu_now;
            last_gpu = gpu_now;
        }
    });

    // Wait for shutdown
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        if stats.should_stop.load(Ordering::Relaxed) {
            break;
        }
    }

    tracing::info!("Shutting down...");

    if let Some(pool) = mining_pool {
        pool.shutdown();
    }

    let _ = std::fs::remove_file(&pid_path);

    // Save cumulative stats to disk
    stats.save_persistent(prior_uptime);

    let snapshot = stats.snapshot();
    tracing::info!(
        total_hashes = snapshot.total_hashes,
        cpu_hashes = snapshot.cpu_hashes,
        gpu_hashes = snapshot.gpu_hashes,
        shares_submitted = snapshot.shares_submitted,
        shares_accepted = snapshot.shares_accepted,
        blocks_found = snapshot.blocks_found,
        uptime_secs = snapshot.uptime_secs,
        "Final stats (saved to disk)"
    );
}

fn init_logging(config: &MinerConfig) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.logging.level));

    if let Some(ref log_file) = config.logging.file {
        let file_path = std::path::PathBuf::from(log_file);
        let dir = file_path.parent().unwrap_or(std::path::Path::new("."));
        let file_name = file_path
            .file_name()
            .unwrap_or(std::ffi::OsStr::new("mi-miner.log"));

        let file_appender = tracing_appender::rolling::never(dir, file_name);
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(non_blocking)
            .init();

        // Leak the guard so it lives for the process lifetime
        std::mem::forget(guard);
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
}

fn daemonize() {
    let home = mi_core::config::dirs_path();
    let _ = std::fs::create_dir_all(&home);

    let pid_path = home.join("mi-miner.pid");

    let daemonize = daemonize::Daemonize::new()
        .pid_file(&pid_path)
        .working_directory(&home);

    match daemonize.start() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("Failed to daemonize: {e}");
            std::process::exit(1);
        }
    }
}

fn stop_daemon() {
    let pid_path = mi_core::config::dirs_path().join("mi-miner.pid");
    match std::fs::read_to_string(&pid_path) {
        Ok(pid_str) => {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                send_sigterm(pid);
                println!("Sent stop signal to mi-miner (PID {pid})");
                let _ = std::fs::remove_file(&pid_path);
            } else {
                eprintln!("Invalid PID in {}", pid_path.display());
            }
        }
        Err(_) => {
            eprintln!("No running mi-miner found (no PID file)");
        }
    }
}

#[cfg(unix)]
fn send_sigterm(pid: i32) {
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: i32) {
    eprintln!("Signal sending not supported on this platform");
}

fn generate_wallet() {
    eprint!("Enter a passphrase to encrypt the recovery phrase: ");
    let passphrase = rpassword::read_password().unwrap_or_default();
    if passphrase.len() < 8 {
        eprintln!("Passphrase must be at least 8 characters.");
        std::process::exit(1);
    }
    eprint!("Confirm passphrase: ");
    let confirm = rpassword::read_password().unwrap_or_default();
    if passphrase != confirm {
        eprintln!("Passphrases do not match.");
        std::process::exit(1);
    }

    match mi_core::wallet::generate_wallet(&passphrase) {
        Ok(info) => {
            println!("=== New Bitcoin Wallet Generated ===\n");
            println!("Address:  {}\n", info.address);
            println!("Recovery phrase (12 words):\n");
            println!("  {}\n", info.mnemonic);
            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║  WRITE DOWN THESE 12 WORDS AND STORE THEM SAFELY.           ║");
            println!("║  If you lose them and find a block, the BTC is GONE FOREVER. ║");
            println!("║  Do NOT share them with anyone.                             ║");
            println!("╚══════════════════════════════════════════════════════════════╝\n");
            println!("Wallet saved to: {}", info.path.display());
            println!("File permissions set to owner-only (600).\n");
            println!("This address will be used automatically when mining.");
            println!("You can also set it manually in ~/.mi-miner/config.toml under [stratum].worker");
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn show_wallet() {
    match mi_core::wallet::load_wallet() {
        Ok(info) => {
            println!("=== mi-miner Wallet ===\n");
            println!("Address:  {}\n", info.address);
            println!("Recovery phrase:\n");
            println!("  {}\n", info.mnemonic);
            println!("Wallet file: {}", info.path.display());
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }
}

fn install_launchd_service() {
    let exe = std::env::current_exe().unwrap();
    let config_path = MinerConfig::default_path();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.mi-miner.daemon</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--config</string>
        <string>{config}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/mi-miner.stdout.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/mi-miner.stderr.log</string>
</dict>
</plist>"#,
        exe = exe.display(),
        config = config_path.display(),
        home = mi_core::config::dirs_path().display(),
    );

    let home = std::env::var("HOME").unwrap_or_default();
    let plist_path =
        PathBuf::from(&home).join("Library/LaunchAgents/com.mi-miner.daemon.plist");

    println!("Launchd plist would be written to: {}", plist_path.display());
    println!("Contents:\n{plist}");
    println!("\nTo install, save this plist and run:");
    println!("  launchctl load {}", plist_path.display());
}

fn fire_macos_notification(block_num: u64) {
    let script = format!(
        r#"display notification "Block #{} found! Check your wallet for ~3.125 BTC." with title "mi-miner: BLOCK FOUND!" sound name "Glass""#,
        block_num
    );
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .spawn()
        .ok();
}
