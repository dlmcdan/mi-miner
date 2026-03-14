use axum::extract::State;
use axum::response::Html;
use axum::Json;
use mi_core::config::MinerConfig;
use mi_core::stats::StatsSnapshot;
use mi_core::MiningStats;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub type AppState = Arc<MiningStats>;

pub async fn index() -> Html<&'static str> {
    Html(include_str!("assets/index.html"))
}

pub async fn stats_json(State(stats): State<AppState>) -> Json<StatsSnapshot> {
    Json(stats.snapshot())
}

// ── Wallet ──

#[derive(Serialize)]
pub struct WalletStatus {
    pub exists: bool,
    pub address: Option<String>,
}

pub async fn wallet_status() -> Json<WalletStatus> {
    match mi_core::wallet::load_wallet() {
        Ok(info) => Json(WalletStatus {
            exists: true,
            address: Some(info.address),
        }),
        Err(_) => Json(WalletStatus {
            exists: false,
            address: None,
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

pub async fn wallet_generate() -> Json<WalletGenerated> {
    match mi_core::wallet::generate_wallet() {
        Ok(info) => Json(WalletGenerated {
            success: true,
            address: Some(info.address),
            mnemonic: Some(info.mnemonic),
            error: None,
        }),
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

pub async fn wallet_set_address(Json(req): Json<SetAddressRequest>) -> Json<SetAddressResponse> {
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

    // Save address to config
    let path = MinerConfig::default_path();
    let mut config = if path.exists() {
        MinerConfig::load(&path).unwrap_or_default()
    } else {
        MinerConfig::default()
    };

    config.stratum.worker = format!("{address}.mi-miner");

    match config.save(&path) {
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

pub async fn mining_pause(State(stats): State<AppState>) -> Json<ControlResponse> {
    stats.paused.store(true, Ordering::Relaxed);
    tracing::info!("Mining paused via dashboard");
    Json(ControlResponse {
        success: true,
        state: "paused".to_string(),
    })
}

pub async fn mining_resume(State(stats): State<AppState>) -> Json<ControlResponse> {
    stats.paused.store(false, Ordering::Relaxed);
    tracing::info!("Mining resumed via dashboard");
    Json(ControlResponse {
        success: true,
        state: "running".to_string(),
    })
}

pub async fn mining_stop(State(stats): State<AppState>) -> Json<ControlResponse> {
    tracing::info!("Miner shutdown requested via dashboard");
    stats.should_stop.store(true, Ordering::Relaxed);
    Json(ControlResponse {
        success: true,
        state: "stopping".to_string(),
    })
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
    }
}

pub async fn config_get() -> Json<ConfigResponse> {
    let path = MinerConfig::default_path();
    let config = if path.exists() {
        match MinerConfig::load(&path) {
            Ok(c) => c,
            Err(e) => {
                return Json(ConfigResponse {
                    success: false,
                    config: None,
                    error: Some(format!("Failed to load config: {e}")),
                })
            }
        }
    } else {
        MinerConfig::default()
    };

    Json(ConfigResponse {
        success: true,
        config: Some(ConfigData::from(&config)),
        error: None,
    })
}

pub async fn config_save(Json(data): Json<ConfigData>) -> Json<ConfigResponse> {
    let path = MinerConfig::default_path();
    let mut config = if path.exists() {
        MinerConfig::load(&path).unwrap_or_default()
    } else {
        MinerConfig::default()
    };

    data.apply_to(&mut config);

    match config.save(&path) {
        Ok(()) => {
            tracing::info!("Config saved via dashboard");
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
            let (_, hashes) = hash_range_midstate(&header, nonce, end, &target, &stop, chunk);
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
            "log_level": "info"
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
}
