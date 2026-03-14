use serde::Serialize;

/// Detected hardware capabilities.
#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub cpu_cores_total: usize,
    pub cpu_cores_performance: usize,
    pub gpu_available: bool,
    pub gpu_name: Option<String>,
    pub memory_gb: u64,
    pub platform: String,
}

/// Detect hardware and return capabilities.
pub fn detect() -> HardwareInfo {
    let total_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    let p_cores = detect_p_cores(total_cores);
    let gpu = detect_gpu();
    let memory_gb = detect_memory();

    HardwareInfo {
        cpu_cores_total: total_cores,
        cpu_cores_performance: p_cores,
        gpu_available: gpu.0,
        gpu_name: gpu.1,
        memory_gb,
        platform: std::env::consts::OS.to_string(),
    }
}

/// Recommended config based on detected hardware.
pub fn auto_configure() -> crate::config::MinerConfig {
    let hw = detect();
    let mut config = crate::config::MinerConfig::default();

    // CPU: use performance cores only
    config.mining.threads = hw.cpu_cores_performance;

    // GPU: enable if available
    config.gpu.enabled = hw.gpu_available;
    config.mining.cpu_only = !hw.gpu_available;

    // If GPU available, set reasonable defaults
    if hw.gpu_available {
        config.gpu.intensity = 1.0;
        config.gpu.batch_size_log2 = 24;
    }

    // Activity throttling: adjust min threads based on core count
    config.activity.min_threads = 1.max(hw.cpu_cores_performance / 4);

    config
}

fn detect_p_cores(total: usize) -> usize {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sysctl")
            .args(["-n", "hw.perflevel0.logicalcpu"])
            .output()
        {
            if let Ok(s) = String::from_utf8(output.stdout) {
                if let Ok(n) = s.trim().parse::<usize>() {
                    return n;
                }
            }
        }
    }
    // Fallback: assume ~75% are performance cores
    (total * 3 / 4).max(1)
}

fn detect_gpu() -> (bool, Option<String>) {
    #[cfg(target_os = "macos")]
    {
        // Check if Metal shader was compiled (Xcode available)
        let metallib_compiled = option_env!("MI_METALLIB_PATH").is_some();

        // Also check if we can find a metallib next to the binary
        let metallib_exists = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sha256d.metallib").exists()))
            .unwrap_or(false);

        let available = metallib_compiled || metallib_exists;

        // Get GPU name via system_profiler
        let name = std::process::Command::new("system_profiler")
            .args(["SPDisplaysDataType", "-detailLevel", "mini"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                s.lines()
                    .find(|l| l.contains("Chipset Model:") || l.contains("Chip Model:"))
                    .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
            });

        // GPU hardware is there even if shader isn't compiled
        let gpu_name = name.or_else(|| {
            // On Apple Silicon, the GPU is always present
            std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| format!("{} (integrated)", s.trim()))
        });

        return (available, gpu_name);
    }

    #[cfg(not(target_os = "macos"))]
    {
        (false, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_returns_valid_info() {
        let hw = detect();
        assert!(hw.cpu_cores_total >= 1);
        assert!(hw.cpu_cores_performance >= 1);
        assert!(hw.cpu_cores_performance <= hw.cpu_cores_total);
        assert!(!hw.platform.is_empty());
    }

    #[test]
    fn test_auto_configure_produces_valid_config() {
        let config = auto_configure();
        assert!(config.mining.threads >= 1);
        assert!(config.activity.min_threads >= 1);
        assert!(config.activity.min_threads <= config.mining.threads);
        // gpu_enabled matches cpu_only being the inverse
        assert_eq!(config.mining.cpu_only, !config.gpu.enabled);
    }

    #[test]
    fn test_hardware_info_serializes() {
        let hw = detect();
        let json = serde_json::to_string(&hw).unwrap();
        assert!(json.contains("cpu_cores_total"));
        assert!(json.contains("platform"));
    }
}

fn detect_memory() -> u64 {
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
        {
            if let Ok(s) = String::from_utf8(output.stdout) {
                if let Ok(bytes) = s.trim().parse::<u64>() {
                    return bytes / (1024 * 1024 * 1024);
                }
            }
        }
    }
    0
}
