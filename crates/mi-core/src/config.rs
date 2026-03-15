use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinerConfig {
    #[serde(default = "default_mining")]
    pub mining: MiningConfig,
    #[serde(default = "default_gpu")]
    pub gpu: GpuConfig,
    #[serde(default)]
    pub stratum: StratumConfig,
    #[serde(default)]
    pub rpc: RpcConfig,
    #[serde(default = "default_web")]
    pub web: WebConfig,
    #[serde(default = "default_activity")]
    pub activity: ActivityConfig,
    #[serde(default = "default_logging")]
    pub logging: LoggingConfig,
    #[serde(default = "default_reward_sharing")]
    pub reward_sharing: RewardSharingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningConfig {
    pub threads: usize,
    pub cpu_only: bool,
    pub gpu_only: bool,
    #[serde(default = "default_electricity_cost")]
    pub electricity_cost_kwh: f64,
}

fn default_electricity_cost() -> f64 {
    0.12
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GpuConfig {
    pub enabled: bool,
    pub intensity: f32,
    pub batch_size_log2: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StratumConfig {
    pub url: String,
    pub worker: String,
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcConfig {
    pub url: String,
    pub user: String,
    pub password: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    pub bind: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityConfig {
    pub enabled: bool,
    pub idle_timeout_secs: u64,
    pub min_threads: usize,
    pub min_gpu_intensity: f32,
    pub ramp_up_secs: u64,
    pub ramp_down_secs: u64,
    pub cpu_threshold: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
    pub file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RewardSharingConfig {
    pub enabled: bool,
    pub percentage: f64,
    pub developer_address: String,
}

fn default_reward_sharing() -> RewardSharingConfig {
    RewardSharingConfig {
        enabled: true,
        percentage: 1.0,
        developer_address: "bc1qt8le6z4p0t5q4qtzsmxhwt7cxmu3ycyzpx77h0".to_string(),
    }
}

fn default_mining() -> MiningConfig {
    MiningConfig {
        threads: num_p_cores(),
        cpu_only: false,
        gpu_only: false,
        electricity_cost_kwh: default_electricity_cost(),
    }
}

fn default_gpu() -> GpuConfig {
    GpuConfig {
        enabled: true,
        intensity: 1.0,
        batch_size_log2: 24,
    }
}

fn default_web() -> WebConfig {
    WebConfig {
        bind: "127.0.0.1:7878".to_string(),
        enabled: true,
    }
}

fn default_activity() -> ActivityConfig {
    ActivityConfig {
        enabled: true,
        idle_timeout_secs: 120,
        min_threads: 1,
        min_gpu_intensity: 0.1,
        ramp_up_secs: 30,
        ramp_down_secs: 5,
        cpu_threshold: 50.0,
    }
}

fn default_logging() -> LoggingConfig {
    LoggingConfig {
        level: "info".to_string(),
        file: None,
    }
}

impl Default for StratumConfig {
    fn default() -> Self {
        Self {
            url: "stratum+tcp://solo.ckpool.org:3333".to_string(),
            worker: "YOUR_BITCOIN_ADDRESS.mi-miner".to_string(),
            password: "x".to_string(),
        }
    }
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8332".to_string(),
            user: "rpcuser".to_string(),
            password: "rpcpassword".to_string(),
            enabled: false,
        }
    }
}

impl Default for MinerConfig {
    fn default() -> Self {
        Self {
            mining: default_mining(),
            gpu: default_gpu(),
            stratum: StratumConfig::default(),
            rpc: RpcConfig::default(),
            web: default_web(),
            activity: default_activity(),
            logging: default_logging(),
            reward_sharing: default_reward_sharing(),
        }
    }
}

impl MinerConfig {
    pub fn load(path: &Path) -> Result<Self, crate::MiMinerError> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| crate::MiMinerError::Config(format!("Failed to read config: {e}")))?;
        let config: Self = toml::from_str(&contents)
            .map_err(|e| crate::MiMinerError::Config(format!("Failed to parse config: {e}")))?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<(), crate::MiMinerError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| crate::MiMinerError::Config(format!("Failed to create dir: {e}")))?;
        }
        let contents = toml::to_string_pretty(self)
            .map_err(|e| crate::MiMinerError::Config(format!("Failed to serialize config: {e}")))?;
        std::fs::write(path, contents)
            .map_err(|e| crate::MiMinerError::Config(format!("Failed to write config: {e}")))?;
        Ok(())
    }

    pub fn default_path() -> PathBuf {
        dirs_path().join("config.toml")
    }
}

pub fn dirs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".mi-miner")
}

fn num_p_cores() -> usize {
    // On M4 Max, 12 P-cores + 4 E-cores = 16. Default to P-cores only.
    // sysctl hw.perflevel0.logicalcpu gives P-core count on macOS.
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("sysctl")
            .arg("-n")
            .arg("hw.perflevel0.logicalcpu")
            .output()
        {
            if let Ok(s) = String::from_utf8(output.stdout) {
                if let Ok(n) = s.trim().parse::<usize>() {
                    return n;
                }
            }
        }
    }
    // Fallback: use total cores minus 4 (rough E-core estimate), min 1
    let total = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    (total.saturating_sub(4)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_roundtrip() {
        let config = MinerConfig::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: MinerConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.stratum.url, config.stratum.url);
        assert_eq!(deserialized.web.bind, config.web.bind);
    }

    #[test]
    fn test_partial_config_parse() {
        let toml_str = r#"
[stratum]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "bc1qtest.worker1"
password = "x"
"#;
        let config: MinerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.stratum.worker, "bc1qtest.worker1");
        assert!(config.gpu.enabled);
        assert_eq!(config.web.bind, "127.0.0.1:7878");
    }

    #[test]
    fn test_empty_config_uses_all_defaults() {
        let config: MinerConfig = toml::from_str("").unwrap();
        assert!(config.mining.threads >= 1);
        assert!(config.gpu.enabled);
        assert_eq!(config.gpu.intensity, 1.0);
        assert_eq!(config.activity.idle_timeout_secs, 120);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("mi-miner-test-config");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_config.toml");

        let config = MinerConfig::default();
        config.save(&path).unwrap();

        let loaded = MinerConfig::load(&path).unwrap();
        assert_eq!(loaded.stratum.url, config.stratum.url);
        assert_eq!(loaded.mining.threads, config.mining.threads);
        assert_eq!(loaded.web.bind, config.web.bind);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn test_load_nonexistent_file_errors() {
        let result = MinerConfig::load(std::path::Path::new("/tmp/nonexistent_mi_miner.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn test_default_path_contains_mi_miner() {
        let path = MinerConfig::default_path();
        assert!(path.to_str().unwrap().contains(".mi-miner"));
        assert!(path.to_str().unwrap().ends_with("config.toml"));
    }

    #[test]
    fn test_dirs_path_contains_mi_miner() {
        let path = dirs_path();
        assert!(path.to_str().unwrap().contains(".mi-miner"));
    }

    #[test]
    fn test_gpu_intensity_in_config() {
        let toml_str = r#"
[gpu]
enabled = true
intensity = 0.5
batch_size_log2 = 24
"#;
        let config: MinerConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.gpu.intensity, 0.5);
    }

    #[test]
    fn test_activity_defaults() {
        let config = MinerConfig::default();
        assert!(config.activity.enabled);
        assert_eq!(config.activity.ramp_up_secs, 30);
        assert_eq!(config.activity.ramp_down_secs, 5);
        assert_eq!(config.activity.cpu_threshold, 50.0);
    }

    #[test]
    fn test_reward_sharing_defaults() {
        let config = MinerConfig::default();
        assert!(config.reward_sharing.enabled);
        assert_eq!(config.reward_sharing.percentage, 1.0);
        assert!(config.reward_sharing.developer_address.starts_with("bc1q"));
    }

    #[test]
    fn test_reward_sharing_toml_roundtrip() {
        let config = MinerConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("[reward_sharing]"));
        assert!(toml_str.contains("percentage = 1.0"));

        let parsed: MinerConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.reward_sharing.enabled, config.reward_sharing.enabled);
        assert_eq!(parsed.reward_sharing.percentage, config.reward_sharing.percentage);
        assert_eq!(parsed.reward_sharing.developer_address, config.reward_sharing.developer_address);
    }

    #[test]
    fn test_old_config_without_reward_sharing_uses_defaults() {
        // Simulate an old config file that doesn't have [reward_sharing]
        let toml_str = r#"
[stratum]
url = "stratum+tcp://solo.ckpool.org:3333"
worker = "bc1qtest.mi-miner"
password = "x"
"#;
        let config: MinerConfig = toml::from_str(toml_str).unwrap();
        // Should use defaults
        assert!(config.reward_sharing.enabled);
        assert_eq!(config.reward_sharing.percentage, 1.0);
    }

    #[test]
    fn test_electricity_cost_default() {
        let config = MinerConfig::default();
        assert_eq!(config.mining.electricity_cost_kwh, 0.12);
    }
}
