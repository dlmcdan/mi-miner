use axum::extract::{Query, State};
use axum::response::Html;
use axum::Json;
use mi_core::config::MinerConfig;
use mi_core::stats::StatsSnapshot;
use mi_core::{LiveConfig, MiningStats};
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub stats: Arc<MiningStats>,
    pub live_config: Arc<LiveConfig>,
    pub block_events: tokio::sync::broadcast::Sender<u64>,
}

pub async fn index() -> Html<&'static str> {
    Html(include_str!("assets/index.html"))
}

pub async fn stats_json(State(state): State<AppState>) -> Json<StatsSnapshot> {
    Json(state.stats.snapshot())
}

// ── Wallet ──

#[derive(Serialize)]
pub struct WalletStatus {
    pub exists: bool,
    pub address: Option<String>,
    pub has_mnemonic: bool,
}

pub async fn wallet_status() -> Json<WalletStatus> {
    match mi_core::wallet::load_wallet() {
        Ok(info) => Json(WalletStatus {
            exists: true,
            address: Some(info.address),
            has_mnemonic: mi_core::wallet::has_encrypted_mnemonic(),
        }),
        Err(_) => Json(WalletStatus {
            exists: false,
            address: None,
            has_mnemonic: false,
        }),
    }
}

#[derive(Serialize)]
pub struct WalletGenerated {
    pub success: bool,
    pub address: Option<String>,
    pub mnemonic: Option<String>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
pub struct PassphraseRequest {
    pub passphrase: String,
}

pub async fn wallet_generate(
    State(state): State<AppState>,
    Json(req): Json<PassphraseRequest>,
) -> Json<WalletGenerated> {
    match mi_core::wallet::generate_wallet(&req.passphrase) {
        Ok(info) => {
            let mut config = state.live_config.snapshot();
            config.stratum.worker = format!("{}.mi-miner", info.address);
            let _ = state.live_config.update(config);

            Json(WalletGenerated {
                success: true,
                address: Some(info.address),
                mnemonic: Some(info.mnemonic),
                error: None,
            })
        }
        Err(e) => Json(WalletGenerated {
            success: false,
            address: None,
            mnemonic: None,
            error: Some(e.to_string()),
        }),
    }
}

pub async fn wallet_mnemonic(Json(req): Json<PassphraseRequest>) -> Json<WalletGenerated> {
    match mi_core::wallet::get_mnemonic(&req.passphrase) {
        Ok(mnemonic) => {
            let address = mi_core::wallet::get_wallet_address();
            Json(WalletGenerated {
                success: true,
                address,
                mnemonic: Some(mnemonic),
                error: None,
            })
        }
        Err(e) => Json(WalletGenerated {
            success: false,
            address: None,
            mnemonic: None,
            error: Some(e.to_string()),
        }),
    }
}

#[derive(Deserialize)]
pub struct RestoreWalletRequest {
    pub mnemonic: String,
    pub passphrase: String,
}

pub async fn wallet_restore(
    State(state): State<AppState>,
    Json(req): Json<RestoreWalletRequest>,
) -> Json<WalletGenerated> {
    let mnemonic = req.mnemonic.trim();

    match mi_core::wallet::restore_wallet(mnemonic, &req.passphrase) {
        Ok(info) => {
            // Update stratum worker config with the restored address
            let mut config = state.live_config.snapshot();
            config.stratum.worker = format!("{}.mi-miner", info.address);
            let _ = state.live_config.update(config);

            tracing::info!("Wallet restored: {}", info.address);
            Json(WalletGenerated {
                success: true,
                address: Some(info.address),
                mnemonic: Some(info.mnemonic),
                error: None,
            })
        }
        Err(e) => Json(WalletGenerated {
            success: false,
            address: None,
            mnemonic: None,
            error: Some(e.to_string()),
        }),
    }
}

// ── Set Existing Address ──

#[derive(Deserialize)]
pub struct SetAddressRequest {
    pub address: String,
}

#[derive(Serialize)]
pub struct SetAddressResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn wallet_set_address(
    State(state): State<AppState>,
    Json(req): Json<SetAddressRequest>,
) -> Json<SetAddressResponse> {
    let address = req.address.trim().to_string();

    // Basic validation: must start with a valid Bitcoin address prefix
    if !address.starts_with("bc1")
        && !address.starts_with("1")
        && !address.starts_with("3")
        && !address.starts_with("tb1")
    {
        return Json(SetAddressResponse {
            success: false,
            error: Some("Invalid Bitcoin address. Expected an address starting with bc1, 1, 3, or tb1.".to_string()),
        });
    }

    if address.len() < 26 || address.len() > 90 {
        return Json(SetAddressResponse {
            success: false,
            error: Some("Invalid address length.".to_string()),
        });
    }

    // Update live config (also saves to disk)
    let mut config = state.live_config.snapshot();
    config.stratum.worker = format!("{address}.mi-miner");

    match state.live_config.update(config) {
        Ok(()) => {
            tracing::info!("External wallet address set: {address}");
            Json(SetAddressResponse {
                success: true,
                error: None,
            })
        }
        Err(e) => Json(SetAddressResponse {
            success: false,
            error: Some(format!("Failed to save config: {e}")),
        }),
    }
}

// ── Mining Controls ──

#[derive(Serialize)]
pub struct ControlResponse {
    pub success: bool,
    pub state: String,
}

pub async fn mining_pause(State(state): State<AppState>) -> Json<ControlResponse> {
    state.stats.paused.store(true, Ordering::Relaxed);
    tracing::info!("Mining paused via dashboard");
    Json(ControlResponse {
        success: true,
        state: "paused".to_string(),
    })
}

pub async fn mining_resume(State(state): State<AppState>) -> Json<ControlResponse> {
    state.stats.paused.store(false, Ordering::Relaxed);
    tracing::info!("Mining resumed via dashboard");
    Json(ControlResponse {
        success: true,
        state: "running".to_string(),
    })
}

pub async fn mining_stop(State(state): State<AppState>) -> Json<ControlResponse> {
    tracing::info!("Miner shutdown requested via dashboard");
    state.stats.should_stop.store(true, Ordering::Relaxed);
    Json(ControlResponse {
        success: true,
        state: "stopping".to_string(),
    })
}

// ── Optimization Check ──

pub async fn optimization_check() -> Json<Vec<mi_core::hardware::OptimizationWarning>> {
    let path = MinerConfig::default_path();
    let config = if path.exists() {
        MinerConfig::load(&path).unwrap_or_default()
    } else {
        MinerConfig::default()
    };
    Json(mi_core::hardware::check_optimization(&config))
}

// ── Configuration ──

#[derive(Serialize)]
pub struct ConfigResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<ConfigData>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ConfigData {
    pub mining_threads: usize,
    pub cpu_only: bool,
    pub gpu_only: bool,
    pub gpu_enabled: bool,
    pub gpu_intensity: f32,
    pub gpu_batch_size_log2: u32,
    pub stratum_url: String,
    pub stratum_worker: String,
    pub stratum_password: String,
    pub web_bind: String,
    pub web_enabled: bool,
    pub activity_enabled: bool,
    pub activity_idle_timeout_secs: u64,
    pub activity_min_threads: usize,
    pub activity_min_gpu_intensity: f32,
    pub activity_ramp_up_secs: u64,
    pub activity_ramp_down_secs: u64,
    pub activity_cpu_threshold: f32,
    pub log_level: String,
    pub electricity_cost_kwh: f64,
}

impl From<&MinerConfig> for ConfigData {
    fn from(c: &MinerConfig) -> Self {
        Self {
            mining_threads: c.mining.threads,
            cpu_only: c.mining.cpu_only,
            gpu_only: c.mining.gpu_only,
            gpu_enabled: c.gpu.enabled,
            gpu_intensity: c.gpu.intensity,
            gpu_batch_size_log2: c.gpu.batch_size_log2,
            stratum_url: c.stratum.url.clone(),
            stratum_worker: c.stratum.worker.clone(),
            stratum_password: c.stratum.password.clone(),
            web_bind: c.web.bind.clone(),
            web_enabled: c.web.enabled,
            activity_enabled: c.activity.enabled,
            activity_idle_timeout_secs: c.activity.idle_timeout_secs,
            activity_min_threads: c.activity.min_threads,
            activity_min_gpu_intensity: c.activity.min_gpu_intensity,
            activity_ramp_up_secs: c.activity.ramp_up_secs,
            activity_ramp_down_secs: c.activity.ramp_down_secs,
            activity_cpu_threshold: c.activity.cpu_threshold,
            log_level: c.logging.level.clone(),
            electricity_cost_kwh: c.mining.electricity_cost_kwh,
        }
    }
}

impl ConfigData {
    fn apply_to(&self, c: &mut MinerConfig) {
        c.mining.threads = self.mining_threads;
        c.mining.cpu_only = self.cpu_only;
        c.mining.gpu_only = self.gpu_only;
        c.gpu.enabled = self.gpu_enabled;
        c.gpu.intensity = self.gpu_intensity;
        c.gpu.batch_size_log2 = self.gpu_batch_size_log2;
        c.stratum.url = self.stratum_url.clone();
        c.stratum.worker = self.stratum_worker.clone();
        c.stratum.password = self.stratum_password.clone();
        c.web.bind = self.web_bind.clone();
        c.web.enabled = self.web_enabled;
        c.activity.enabled = self.activity_enabled;
        c.activity.idle_timeout_secs = self.activity_idle_timeout_secs;
        c.activity.min_threads = self.activity_min_threads;
        c.activity.min_gpu_intensity = self.activity_min_gpu_intensity;
        c.activity.ramp_up_secs = self.activity_ramp_up_secs;
        c.activity.ramp_down_secs = self.activity_ramp_down_secs;
        c.activity.cpu_threshold = self.activity_cpu_threshold;
        c.logging.level = self.log_level.clone();
        c.mining.electricity_cost_kwh = self.electricity_cost_kwh;
    }
}

pub async fn config_get(State(state): State<AppState>) -> Json<ConfigResponse> {
    let config = state.live_config.snapshot();
    Json(ConfigResponse {
        success: true,
        config: Some(ConfigData::from(&config)),
        error: None,
    })
}

pub async fn config_save(
    State(state): State<AppState>,
    Json(data): Json<ConfigData>,
) -> Json<ConfigResponse> {
    let mut config = state.live_config.snapshot();
    data.apply_to(&mut config);

    match state.live_config.update(config.clone()) {
        Ok(()) => {
            tracing::info!("Config saved and applied live");
            Json(ConfigResponse {
                success: true,
                config: Some(ConfigData::from(&config)),
                error: None,
            })
        }
        Err(e) => Json(ConfigResponse {
            success: false,
            config: None,
            error: Some(format!("Failed to save config: {e}")),
        }),
    }
}

// ── Hardware Detection & Auto Configure ──

pub async fn hardware_info() -> Json<mi_core::hardware::HardwareInfo> {
    Json(mi_core::hardware::detect())
}

#[derive(Serialize)]
pub struct AutoConfigResponse {
    pub success: bool,
    pub config: Option<ConfigData>,
    pub hardware: mi_core::hardware::HardwareInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub async fn auto_configure() -> Json<AutoConfigResponse> {
    let hw = mi_core::hardware::detect();
    let recommended = mi_core::hardware::auto_configure();
    let path = MinerConfig::default_path();

    // Load existing config to preserve stratum/wallet settings
    let mut config = if path.exists() {
        MinerConfig::load(&path).unwrap_or_default()
    } else {
        MinerConfig::default()
    };

    // Apply auto-detected hardware settings but keep user's stratum/wallet/web settings
    config.mining.threads = recommended.mining.threads;
    config.mining.cpu_only = recommended.mining.cpu_only;
    config.mining.gpu_only = false;
    config.gpu.enabled = recommended.gpu.enabled;
    config.gpu.intensity = recommended.gpu.intensity;
    config.gpu.batch_size_log2 = recommended.gpu.batch_size_log2;
    config.activity.min_threads = recommended.activity.min_threads;

    match config.save(&path) {
        Ok(()) => {
            tracing::info!("Auto-configured for detected hardware");
            Json(AutoConfigResponse {
                success: true,
                config: Some(ConfigData::from(&config)),
                hardware: hw,
                error: None,
            })
        }
        Err(e) => Json(AutoConfigResponse {
            success: false,
            config: None,
            hardware: hw,
            error: Some(format!("Failed to save config: {e}")),
        }),
    }
}

// ── Test: Connection ──

#[derive(Serialize)]
pub struct TestResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

pub async fn test_connection() -> Json<TestResult> {
    let path = MinerConfig::default_path();
    let config = if path.exists() {
        MinerConfig::load(&path).unwrap_or_default()
    } else {
        MinerConfig::default()
    };

    let addr = config
        .stratum
        .url
        .trim_start_matches("stratum+tcp://")
        .trim_start_matches("stratum://");

    match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(_stream)) => Json(TestResult {
            success: true,
            message: format!("Connected to {addr}"),
            details: None,
        }),
        Ok(Err(e)) => Json(TestResult {
            success: false,
            message: "Connection failed".to_string(),
            details: Some(format!("{e}")),
        }),
        Err(_) => Json(TestResult {
            success: false,
            message: "Connection timed out (5s)".to_string(),
            details: Some(addr.to_string()),
        }),
    }
}

// ── Test: Quick Benchmark ──

pub async fn test_benchmark() -> Json<TestResult> {
    let result = tokio::task::spawn_blocking(|| {
        use mi_mining::hasher::hash_range_midstate;
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};

        let mut header = [0u8; 80];
        header[0..4].copy_from_slice(&0x20000000i32.to_le_bytes());
        header[4..36].fill(0xaa);
        header[36..68].fill(0xbb);
        header[68..72].copy_from_slice(&1700000000u32.to_le_bytes());
        header[72..76].copy_from_slice(&0x1d00ffffu32.to_le_bytes());
        let target = [0u8; 32];
        let stop = AtomicBool::new(false);

        let start = Instant::now();
        let deadline = Duration::from_secs(3);
        let mut total: u64 = 0;
        let mut nonce = 0u32;
        let chunk = 1u32 << 20;

        while start.elapsed() < deadline {
            let end = nonce.saturating_add(chunk);
            let (_, hashes) = hash_range_midstate(&header, nonce, end, &target, &stop, chunk, None);
            total += hashes;
            nonce = end;
            if nonce == 0 {
                break;
            }
        }

        let elapsed = start.elapsed().as_secs_f64();
        let rate = total as f64 / elapsed;
        (rate, total, elapsed)
    })
    .await;

    match result {
        Ok((rate, total, elapsed)) => {
            let rate_str = if rate >= 1_000_000.0 {
                format!("{:.2} MH/s", rate / 1_000_000.0)
            } else if rate >= 1_000.0 {
                format!("{:.2} KH/s", rate / 1_000.0)
            } else {
                format!("{:.0} H/s", rate)
            };

            Json(TestResult {
                success: true,
                message: format!("Single-core: {rate_str}"),
                details: Some(format!(
                    "{total} hashes in {elapsed:.1}s"
                )),
            })
        }
        Err(e) => Json(TestResult {
            success: false,
            message: "Benchmark failed".to_string(),
            details: Some(format!("{e}")),
        }),
    }
}

// ── Pool Presets ──

#[derive(Serialize, Clone)]
pub struct PoolPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub url: &'static str,
    pub fee_pct: f64,
    pub payout_method: &'static str,
    pub min_payout_btc: f64,
    pub pool_type: &'static str,        // "solo" or "pooled"
    pub lightning: bool,
    pub worker_suffix: &'static str,    // appended to BTC address (e.g., ".mi-miner")
    pub password: &'static str,         // default pool password
    pub description: &'static str,
}

pub fn pool_presets() -> Vec<PoolPreset> {
    vec![
        PoolPreset {
            id: "solo-ckpool",
            name: "Solo CKPool",
            url: "stratum+tcp://solo.ckpool.org:3333",
            fee_pct: 2.0,
            payout_method: "Solo",
            min_payout_btc: 0.0,
            pool_type: "solo",
            lightning: false,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "Solo mining relay — you get the full block reward (~3.125 BTC) minus 2% fee if you find a block. No registration required. Longest track record (since 2014).",
        },
        PoolPreset {
            id: "public-pool",
            name: "Public Pool",
            url: "stratum+tcp://public-pool.io:21496",
            fee_pct: 0.0,
            payout_method: "Solo",
            min_payout_btc: 0.0,
            pool_type: "solo",
            lightning: false,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "Zero-fee solo mining relay — keep 100% of the block reward. Open source. Popular with Bitaxe home miners.",
        },
        PoolPreset {
            id: "ocean",
            name: "OCEAN",
            url: "stratum+tcp://mine.ocean.xyz:3334",
            fee_pct: 2.0,
            payout_method: "TIDES",
            min_payout_btc: 0.01,
            pool_type: "pooled",
            lightning: true,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "Non-custodial pooled mining with transparent TIDES payouts. Strong decentralization ethos. Lightning payouts available for small miners.",
        },
        PoolPreset {
            id: "braiins",
            name: "Braiins Pool",
            url: "stratum+tcp://stratum.braiins.com:3333",
            fee_pct: 2.0,
            payout_method: "FPPS",
            min_payout_btc: 0.001,
            pool_type: "pooled",
            lightning: true,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "The original mining pool (since 2010). 2% FPPS or 0% PPLNS. Lightning payouts with no minimum — ideal for small miners.",
        },
        PoolPreset {
            id: "viabtc",
            name: "ViaBTC",
            url: "stratum+tcp://btc.viabtc.com:3333",
            fee_pct: 4.0,
            payout_method: "PPS+",
            min_payout_btc: 0.001,
            pool_type: "pooled",
            lightning: false,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "Large pool (~14% of network). Switch between PPS+ (4%), PPLNS (2%), and Solo (2%) on the same platform.",
        },
        PoolPreset {
            id: "f2pool",
            name: "F2Pool",
            url: "stratum+tcp://btc.f2pool.com:3333",
            fee_pct: 4.0,
            payout_method: "FPPS",
            min_payout_btc: 0.001,
            pool_type: "pooled",
            lightning: false,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "Operating since 2013. ~10% of network. FPPS (4%), PPS+ (2.5%), or PPLNS (2%).",
        },
        PoolPreset {
            id: "luxor",
            name: "Luxor",
            url: "stratum+tcp://btc.global.luxor.tech:700",
            fee_pct: 0.0,
            payout_method: "FPPS",
            min_payout_btc: 0.001,
            pool_type: "pooled",
            lightning: false,
            worker_suffix: ".mi-miner",
            password: "x",
            description: "US-based, 0% pool fee, FPPS payouts. Low latency for North American miners. SOC 2 compliant.",
        },
    ]
}

pub async fn pools_list() -> Json<Vec<PoolPreset>> {
    Json(pool_presets())
}

// ── QR Code ──

#[derive(Deserialize)]
pub struct QrQuery {
    pub data: String,
}

pub async fn wallet_qr(Query(q): Query<QrQuery>) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use image::Luma;
    use qrcode::QrCode;

    let code = match QrCode::new(q.data.as_bytes()) {
        Ok(c) => c,
        Err(_) => {
            return axum::response::Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(axum::body::Body::empty())
                .unwrap();
        }
    };

    let img = code.render::<Luma<u8>>().quiet_zone(true).build();
    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut png_bytes));
    if image::ImageEncoder::write_image(
        encoder,
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::L8,
    )
    .is_err()
    {
        return axum::response::Response::builder()
            .status(StatusCode::INTERNAL_SERVER_ERROR)
            .body(axum::body::Body::empty())
            .unwrap();
    }

    axum::response::Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "image/png")
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(axum::body::Body::from(png_bytes))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_data_from_miner_config() {
        let config = MinerConfig::default();
        let data = ConfigData::from(&config);
        assert_eq!(data.stratum_url, config.stratum.url);
        assert_eq!(data.mining_threads, config.mining.threads);
        assert_eq!(data.gpu_enabled, config.gpu.enabled);
        assert_eq!(data.gpu_intensity, config.gpu.intensity);
        assert_eq!(data.web_bind, config.web.bind);
        assert_eq!(data.activity_enabled, config.activity.enabled);
        assert_eq!(data.log_level, config.logging.level);
    }

    #[test]
    fn test_config_data_apply_to() {
        let mut config = MinerConfig::default();
        let data = ConfigData {
            mining_threads: 4,
            cpu_only: true,
            gpu_only: false,
            gpu_enabled: false,
            gpu_intensity: 0.5,
            gpu_batch_size_log2: 20,
            stratum_url: "stratum+tcp://test:3333".to_string(),
            stratum_worker: "testworker".to_string(),
            stratum_password: "pass".to_string(),
            web_bind: "0.0.0.0:8080".to_string(),
            web_enabled: true,
            activity_enabled: false,
            activity_idle_timeout_secs: 60,
            activity_min_threads: 2,
            activity_min_gpu_intensity: 0.2,
            activity_ramp_up_secs: 10,
            activity_ramp_down_secs: 3,
            activity_cpu_threshold: 75.0,
            log_level: "debug".to_string(),
            electricity_cost_kwh: 0.15,
        };
        data.apply_to(&mut config);

        assert_eq!(config.mining.threads, 4);
        assert!(config.mining.cpu_only);
        assert!(!config.gpu.enabled);
        assert_eq!(config.gpu.intensity, 0.5);
        assert_eq!(config.stratum.url, "stratum+tcp://test:3333");
        assert_eq!(config.stratum.worker, "testworker");
        assert_eq!(config.web.bind, "0.0.0.0:8080");
        assert!(!config.activity.enabled);
        assert_eq!(config.activity.idle_timeout_secs, 60);
        assert_eq!(config.logging.level, "debug");
    }

    #[test]
    fn test_config_data_roundtrip() {
        let config = MinerConfig::default();
        let data = ConfigData::from(&config);
        let mut config2 = MinerConfig::default();
        data.apply_to(&mut config2);

        assert_eq!(config2.mining.threads, config.mining.threads);
        assert_eq!(config2.gpu.enabled, config.gpu.enabled);
        assert_eq!(config2.stratum.url, config.stratum.url);
    }

    #[test]
    fn test_config_data_serializes() {
        let config = MinerConfig::default();
        let data = ConfigData::from(&config);
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("mining_threads"));
        assert!(json.contains("stratum_url"));
        assert!(json.contains("gpu_intensity"));
    }

    #[test]
    fn test_config_data_deserializes() {
        let json = r#"{
            "mining_threads": 8,
            "cpu_only": false,
            "gpu_only": false,
            "gpu_enabled": true,
            "gpu_intensity": 0.75,
            "gpu_batch_size_log2": 22,
            "stratum_url": "test",
            "stratum_worker": "w",
            "stratum_password": "p",
            "web_bind": "127.0.0.1:7878",
            "web_enabled": true,
            "activity_enabled": true,
            "activity_idle_timeout_secs": 120,
            "activity_min_threads": 1,
            "activity_min_gpu_intensity": 0.1,
            "activity_ramp_up_secs": 30,
            "activity_ramp_down_secs": 5,
            "activity_cpu_threshold": 50.0,
            "log_level": "info",
            "electricity_cost_kwh": 0.12
        }"#;
        let data: ConfigData = serde_json::from_str(json).unwrap();
        assert_eq!(data.mining_threads, 8);
        assert_eq!(data.gpu_intensity, 0.75);
        assert_eq!(data.gpu_batch_size_log2, 22);
    }

    #[test]
    fn test_wallet_status_serializes() {
        let status = WalletStatus {
            exists: true,
            address: Some("bc1qtest".to_string()),
            has_mnemonic: true,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"exists\":true"));
        assert!(json.contains("bc1qtest"));
    }

    #[test]
    fn test_control_response_serializes() {
        let resp = ControlResponse {
            success: true,
            state: "paused".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"state\":\"paused\""));
    }

    #[test]
    fn test_test_result_serializes() {
        let result = TestResult {
            success: true,
            message: "Connected".to_string(),
            details: Some("extra info".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("Connected"));
        assert!(json.contains("extra info"));
    }

    #[test]
    fn test_test_result_without_details() {
        let result = TestResult {
            success: false,
            message: "Failed".to_string(),
            details: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(!json.contains("details")); // skip_serializing_if = None
    }

    // ── WalletGenerated serialization ──

    #[test]
    fn test_wallet_generated_all_fields_present() {
        let wg = WalletGenerated {
            success: true,
            address: Some("bc1qexample".to_string()),
            mnemonic: Some("word1 word2 word3".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&wg).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("bc1qexample"));
        assert!(json.contains("word1 word2 word3"));
        // error is None but WalletGenerated does not have skip_serializing_if,
        // so it should still appear as null
        assert!(json.contains("\"error\":null"));
    }

    #[test]
    fn test_wallet_generated_error_case() {
        let wg = WalletGenerated {
            success: false,
            address: None,
            mnemonic: None,
            error: Some("something went wrong".to_string()),
        };
        let json = serde_json::to_string(&wg).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"address\":null"));
        assert!(json.contains("\"mnemonic\":null"));
        assert!(json.contains("something went wrong"));
    }

    #[test]
    fn test_wallet_generated_all_none() {
        let wg = WalletGenerated {
            success: false,
            address: None,
            mnemonic: None,
            error: None,
        };
        let json = serde_json::to_string(&wg).unwrap();
        // All optional fields should serialize as null (no skip_serializing_if)
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["address"].is_null());
        assert!(parsed["mnemonic"].is_null());
        assert!(parsed["error"].is_null());
    }

    // ── SetAddressResponse serialization ──

    #[test]
    fn test_set_address_response_success() {
        let resp = SetAddressResponse {
            success: true,
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        // error has skip_serializing_if = None, so should not appear
        assert!(!json.contains("error"));
    }

    #[test]
    fn test_set_address_response_with_error() {
        let resp = SetAddressResponse {
            success: false,
            error: Some("Invalid address".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"Invalid address\""));
    }

    // ── AutoConfigResponse serialization ──

    #[test]
    fn test_auto_config_response_success_serializes() {
        let config_data = ConfigData {
            mining_threads: 16,
            cpu_only: false,
            gpu_only: false,
            gpu_enabled: true,
            gpu_intensity: 0.9,
            gpu_batch_size_log2: 24,
            stratum_url: "stratum+tcp://pool:3333".to_string(),
            stratum_worker: "w".to_string(),
            stratum_password: "x".to_string(),
            web_bind: "127.0.0.1:7878".to_string(),
            web_enabled: true,
            activity_enabled: true,
            activity_idle_timeout_secs: 120,
            activity_min_threads: 2,
            activity_min_gpu_intensity: 0.1,
            activity_ramp_up_secs: 30,
            activity_ramp_down_secs: 5,
            activity_cpu_threshold: 50.0,
            log_level: "info".to_string(),
            electricity_cost_kwh: 0.12,
        };
        let hw = mi_core::hardware::detect();
        let resp = AutoConfigResponse {
            success: true,
            config: Some(config_data),
            hardware: hw,
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"mining_threads\":16"));
        assert!(json.contains("\"hardware\""));
        // error is None with skip_serializing_if, should not appear
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_auto_config_response_error_serializes() {
        let hw = mi_core::hardware::detect();
        let resp = AutoConfigResponse {
            success: false,
            config: None,
            hardware: hw,
            error: Some("Failed to detect".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("Failed to detect"));
        assert!(json.contains("\"config\":null"));
    }

    // ── QrQuery deserialization ──

    #[test]
    fn test_qr_query_deserializes() {
        let json = r#"{"data":"bc1qtest123"}"#;
        let q: QrQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.data, "bc1qtest123");
    }

    #[test]
    fn test_qr_query_empty_data() {
        let json = r#"{"data":""}"#;
        let q: QrQuery = serde_json::from_str(json).unwrap();
        assert_eq!(q.data, "");
    }

    #[test]
    fn test_qr_query_missing_data_fails() {
        let json = r#"{}"#;
        let result = serde_json::from_str::<QrQuery>(json);
        assert!(result.is_err());
    }

    // ── PassphraseRequest deserialization ──

    #[test]
    fn test_passphrase_request_deserializes() {
        let json = r#"{"passphrase":"my secret passphrase"}"#;
        let req: PassphraseRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.passphrase, "my secret passphrase");
    }

    #[test]
    fn test_passphrase_request_empty_passphrase() {
        let json = r#"{"passphrase":""}"#;
        let req: PassphraseRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.passphrase, "");
    }

    #[test]
    fn test_passphrase_request_missing_field_fails() {
        let json = r#"{}"#;
        let result = serde_json::from_str::<PassphraseRequest>(json);
        assert!(result.is_err());
    }

    // ── RestoreWalletRequest deserialization ──

    #[test]
    fn test_restore_wallet_request_deserializes() {
        let json = r#"{"mnemonic":"abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about","passphrase":"test123"}"#;
        let req: RestoreWalletRequest = serde_json::from_str(json).unwrap();
        assert!(req.mnemonic.starts_with("abandon"));
        assert_eq!(req.passphrase, "test123");
    }

    #[test]
    fn test_restore_wallet_request_empty_fields() {
        let json = r#"{"mnemonic":"","passphrase":""}"#;
        let req: RestoreWalletRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.mnemonic, "");
        assert_eq!(req.passphrase, "");
    }

    #[test]
    fn test_restore_wallet_request_missing_mnemonic_fails() {
        let json = r#"{"passphrase":"test"}"#;
        let result = serde_json::from_str::<RestoreWalletRequest>(json);
        assert!(result.is_err());
    }

    #[test]
    fn test_restore_wallet_request_missing_passphrase_fails() {
        let json = r#"{"mnemonic":"word1 word2"}"#;
        let result = serde_json::from_str::<RestoreWalletRequest>(json);
        assert!(result.is_err());
    }

    // ── WalletStatus with has_mnemonic: false ──

    #[test]
    fn test_wallet_status_no_mnemonic_no_address() {
        let status = WalletStatus {
            exists: false,
            address: None,
            has_mnemonic: false,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"exists\":false"));
        assert!(json.contains("\"has_mnemonic\":false"));
        assert!(json.contains("\"address\":null"));
    }

    #[test]
    fn test_wallet_status_exists_but_no_mnemonic() {
        // External address set, no mnemonic stored
        let status = WalletStatus {
            exists: true,
            address: Some("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa".to_string()),
            has_mnemonic: false,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"exists\":true"));
        assert!(json.contains("\"has_mnemonic\":false"));
        assert!(json.contains("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa"));
    }

    #[test]
    fn test_wallet_status_roundtrip() {
        let status = WalletStatus {
            exists: true,
            address: Some("bc1qtest".to_string()),
            has_mnemonic: true,
        };
        let json = serde_json::to_string(&status).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["exists"], true);
        assert_eq!(parsed["address"], "bc1qtest");
        assert_eq!(parsed["has_mnemonic"], true);
    }

    // ── ConfigData edge values ──

    #[test]
    fn test_config_data_zero_threads() {
        let data = ConfigData {
            mining_threads: 0,
            cpu_only: false,
            gpu_only: false,
            gpu_enabled: false,
            gpu_intensity: 0.0,
            gpu_batch_size_log2: 0,
            stratum_url: "".to_string(),
            stratum_worker: "".to_string(),
            stratum_password: "".to_string(),
            web_bind: "".to_string(),
            web_enabled: false,
            activity_enabled: false,
            activity_idle_timeout_secs: 0,
            activity_min_threads: 0,
            activity_min_gpu_intensity: 0.0,
            activity_ramp_up_secs: 0,
            activity_ramp_down_secs: 0,
            activity_cpu_threshold: 0.0,
            log_level: "".to_string(),
            electricity_cost_kwh: 0.0,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"mining_threads\":0"));

        // Verify it can be applied to a MinerConfig
        let mut config = MinerConfig::default();
        data.apply_to(&mut config);
        assert_eq!(config.mining.threads, 0);
        assert_eq!(config.gpu.intensity, 0.0);
    }

    #[test]
    fn test_config_data_max_intensity() {
        let data = ConfigData {
            mining_threads: usize::MAX,
            cpu_only: true,
            gpu_only: true,
            gpu_enabled: true,
            gpu_intensity: 1.0,
            gpu_batch_size_log2: 32,
            stratum_url: "stratum+tcp://pool:3333".to_string(),
            stratum_worker: "w".to_string(),
            stratum_password: "p".to_string(),
            web_bind: "0.0.0.0:9999".to_string(),
            web_enabled: true,
            activity_enabled: true,
            activity_idle_timeout_secs: u64::MAX,
            activity_min_threads: usize::MAX,
            activity_min_gpu_intensity: 1.0,
            activity_ramp_up_secs: u64::MAX,
            activity_ramp_down_secs: u64::MAX,
            activity_cpu_threshold: 100.0,
            log_level: "trace".to_string(),
            electricity_cost_kwh: 0.50,
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"gpu_intensity\":1.0") || json.contains("\"gpu_intensity\":1"));
        assert!(json.contains("\"gpu_batch_size_log2\":32"));

        let mut config = MinerConfig::default();
        data.apply_to(&mut config);
        assert!(config.mining.cpu_only);
        assert!(config.mining.gpu_only);
        assert_eq!(config.gpu.batch_size_log2, 32);
    }

    #[test]
    fn test_config_data_empty_strings() {
        let data = ConfigData {
            mining_threads: 1,
            cpu_only: false,
            gpu_only: false,
            gpu_enabled: false,
            gpu_intensity: 0.5,
            gpu_batch_size_log2: 20,
            stratum_url: "".to_string(),
            stratum_worker: "".to_string(),
            stratum_password: "".to_string(),
            web_bind: "".to_string(),
            web_enabled: false,
            activity_enabled: false,
            activity_idle_timeout_secs: 0,
            activity_min_threads: 0,
            activity_min_gpu_intensity: 0.0,
            activity_ramp_up_secs: 0,
            activity_ramp_down_secs: 0,
            activity_cpu_threshold: 0.0,
            log_level: "".to_string(),
            electricity_cost_kwh: 0.0,
        };

        let mut config = MinerConfig::default();
        data.apply_to(&mut config);
        assert_eq!(config.stratum.url, "");
        assert_eq!(config.stratum.worker, "");
        assert_eq!(config.stratum.password, "");
        assert_eq!(config.web.bind, "");
        assert_eq!(config.logging.level, "");
    }

    #[test]
    fn test_config_data_serialization_roundtrip() {
        let original = ConfigData {
            mining_threads: 12,
            cpu_only: false,
            gpu_only: true,
            gpu_enabled: true,
            gpu_intensity: 0.85,
            gpu_batch_size_log2: 23,
            stratum_url: "stratum+tcp://solo.ckpool.org:3333".to_string(),
            stratum_worker: "bc1qtest.miner".to_string(),
            stratum_password: "x".to_string(),
            web_bind: "127.0.0.1:7878".to_string(),
            web_enabled: true,
            activity_enabled: true,
            activity_idle_timeout_secs: 300,
            activity_min_threads: 4,
            activity_min_gpu_intensity: 0.3,
            activity_ramp_up_secs: 15,
            activity_ramp_down_secs: 3,
            activity_cpu_threshold: 60.0,
            log_level: "warn".to_string(),
            electricity_cost_kwh: 0.18,
        };
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: ConfigData = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.mining_threads, original.mining_threads);
        assert_eq!(deserialized.gpu_only, original.gpu_only);
        assert_eq!(deserialized.gpu_intensity, original.gpu_intensity);
        assert_eq!(deserialized.stratum_url, original.stratum_url);
        assert_eq!(deserialized.activity_idle_timeout_secs, original.activity_idle_timeout_secs);
        assert_eq!(deserialized.log_level, original.log_level);
    }

    // ── ConfigResponse serialization ──

    #[test]
    fn test_config_response_success_skips_error() {
        let resp = ConfigResponse {
            success: true,
            config: Some(ConfigData::from(&MinerConfig::default())),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"config\""));
        // error should be skipped (skip_serializing_if)
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_config_response_error_skips_config() {
        let resp = ConfigResponse {
            success: false,
            config: None,
            error: Some("disk full".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("disk full"));
        // config should be skipped (skip_serializing_if)
        assert!(!json.contains("\"config\""));
    }
}
