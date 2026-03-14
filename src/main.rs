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

    // Auto-use wallet address if worker is still the default placeholder
    let needs_wallet = if config.stratum.worker.starts_with("YOUR_BITCOIN_ADDRESS") {
        if let Some(address) = mi_core::wallet::get_wallet_address() {
            eprintln!("Using wallet address: {address}");
            config.stratum.worker = format!("{address}.mi-miner");
            false
        } else {
            true
        }
    } else {
        false
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

    let pid_path = mi_core::config::dirs_path().join("mi-miner.pid");
    let _ = std::fs::create_dir_all(mi_core::config::dirs_path());
    let _ = std::fs::write(&pid_path, std::process::id().to_string());

    // Signal handler
    let stats_signal = stats.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Shutdown signal received");
        stats_signal.should_stop.store(true, Ordering::Relaxed);
    });

    // GPU manager
    let _gpu_manager = if config.gpu.enabled && !config.mining.cpu_only {
        let mgr = mi_gpu::GpuManager::new(stats.clone(), config.gpu.batch_size_log2);
        if mgr.is_available() {
            tracing::info!("GPU mining: ACTIVE");
            Some(mgr)
        } else {
            tracing::warn!("GPU mining: NOT AVAILABLE (falling back to CPU-only)");
            None
        }
    } else {
        tracing::info!("GPU mining: DISABLED");
        None
    };

    // Channel for share submission to stratum
    let (submit_tx, submit_rx) = tokio::sync::mpsc::channel(64);

    // CPU mining pool — workers hash continuously, polling SharedWork for new jobs
    let submit_tx_mining = submit_tx.clone();
    let mining_pool = if !config.mining.gpu_only {
        let pool = mi_mining::MiningPool::new(
            config.mining.threads,
            stats.clone(),
            Box::new(move |work, nonce, _hash| {
                tracing::info!(nonce = nonce, "CPU found valid nonce!");
                let submission = mi_network::stratum::client::ShareSubmission {
                    job_id: work.job_id.clone(),
                    extranonce2: work.extranonce,
                    ntime: String::new(),
                    nonce,
                };
                let _ = submit_tx_mining.try_send(submission);
            }),
        );
        Some(pool)
    } else {
        None
    };

    // Work distribution: stratum callback updates SharedWork, workers pick it up instantly
    let on_work: Arc<mi_network::stratum::client::WorkCallback> = {
        let (work_tx, work_rx) = std::sync::mpsc::sync_channel::<mi_mining::worker::Work>(2);

        if let Some(ref pool) = mining_pool {
            let shared_work = pool.shared_work();
            let stats = stats.clone();
            std::thread::Builder::new()
                .name("work-updater".to_string())
                .spawn(move || {
                    while let Ok(work) = work_rx.recv() {
                        if stats.should_stop.load(Ordering::Relaxed) {
                            break;
                        }
                        shared_work.update(work);
                    }
                })
                .ok();
        }

        Arc::new(Box::new(
            move |template: mi_mining::block::BlockTemplate, target: [u8; 32]| {
                let (_header, header_bytes) = template.build_header(0);

                let work = mi_mining::worker::Work {
                    header: header_bytes,
                    target,
                    job_id: template.job_id.clone(),
                    extranonce: 0,
                };

                tracing::info!(job = template.job_id, "New work → miners");
                let _ = work_tx.send(work);
            },
        ))
    };

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
        let client = mi_network::StratumClient::new(
            &config.stratum.url,
            &config.stratum.worker,
            &config.stratum.password,
            stats.clone(),
        );

        tokio::spawn(async move {
            if let Err(e) = client.run(on_work, submit_rx).await {
                tracing::error!("Stratum client error: {e}");
            }
        });
    }

    // Activity monitor
    if config.activity.enabled {
        let (throttle_tx, mut throttle_rx) =
            tokio::sync::watch::channel(mi_activity::throttle::ThrottleState {
                target_threads: config.mining.threads,
                target_gpu_intensity: config.gpu.intensity,
                is_ramping: false,
            });

        let monitor = mi_activity::ActivityMonitor::new(
            config.activity.clone(),
            stats.clone(),
            config.mining.threads,
        );

        tokio::spawn(async move {
            if let Err(e) = monitor.run(throttle_tx).await {
                tracing::error!("Activity monitor error: {e}");
            }
        });

        tokio::spawn(async move {
            while throttle_rx.changed().await.is_ok() {
                let state = throttle_rx.borrow().clone();
                tracing::debug!(
                    threads = state.target_threads,
                    gpu_intensity = state.target_gpu_intensity,
                    "Throttle update"
                );
            }
        });
    }

    // Web dashboard
    if config.web.enabled {
        let bind = config.web.bind.clone();
        let stats_web = stats.clone();
        tokio::spawn(async move {
            if let Err(e) = mi_web::start_server(&bind, stats_web).await {
                tracing::error!("Web server error: {e}");
            }
        });
    }

    // Hashrate calculator (1-second rolling window)
    let stats_hr = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        let mut last_cpu = 0u64;
        let mut last_gpu = 0u64;

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

    let snapshot = stats.snapshot();
    tracing::info!(
        total_hashes = snapshot.total_hashes,
        cpu_hashes = snapshot.cpu_hashes,
        gpu_hashes = snapshot.gpu_hashes,
        shares_submitted = snapshot.shares_submitted,
        shares_accepted = snapshot.shares_accepted,
        blocks_found = snapshot.blocks_found,
        uptime_secs = snapshot.uptime_secs,
        "Final stats"
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
    match mi_core::wallet::generate_wallet() {
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
