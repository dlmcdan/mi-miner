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
        // Check if Metal GPU mining is functional by looking for the compiled .metallib
        // in the same places the GPU manager checks at runtime.

        // 1. Check next to the binary
        let metallib_next_to_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("sha256d.metallib").exists()))
            .unwrap_or(false);

        // 2. Check if Metal tools are installed (can compile shaders on rebuild)
        let metal_tools_available = std::process::Command::new("xcrun")
            .args(["-sdk", "macosx", "metal", "-v"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let available = metallib_next_to_exe || metal_tools_available;

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

/// Check current config against detected hardware and return optimization warnings.
pub fn check_optimization(config: &crate::config::MinerConfig) -> Vec<OptimizationWarning> {
    let hw = detect();
    let mut warnings = Vec::new();

    // GPU available but not enabled
    if hw.gpu_available && !config.gpu.enabled {
        warnings.push(OptimizationWarning {
            severity: Severity::High,
            message: "GPU mining is disabled but your GPU supports Metal compute shaders.".to_string(),
            fix: "Enable GPU in Settings or run Auto Configure. GPU can provide 100x+ more hashrate than CPU alone.".to_string(),
        });
    }

    // GPU hardware present but shader not compiled
    if !hw.gpu_available && hw.gpu_name.is_some() {
        warnings.push(OptimizationWarning {
            severity: Severity::High,
            message: format!(
                "GPU detected ({}) but Metal shader is not compiled.",
                hw.gpu_name.as_deref().unwrap_or("unknown")
            ),
            fix: "Install Xcode from the App Store and rebuild: ./scripts/dev.sh".to_string(),
        });
    }

    // Using fewer threads than available P-cores
    if config.mining.threads < hw.cpu_cores_performance && !config.mining.gpu_only {
        warnings.push(OptimizationWarning {
            severity: Severity::Medium,
            message: format!(
                "Using {} CPU threads but {} performance cores are available.",
                config.mining.threads, hw.cpu_cores_performance
            ),
            fix: format!(
                "Increase mining threads to {} in Settings for maximum CPU hashrate.",
                hw.cpu_cores_performance
            ),
        });
    }

    // Using more threads than P-cores (E-cores are slower, diminishing returns)
    if config.mining.threads > hw.cpu_cores_performance && hw.cpu_cores_performance < hw.cpu_cores_total {
        warnings.push(OptimizationWarning {
            severity: Severity::Low,
            message: format!(
                "Using {} threads but only {} are performance cores. Extra threads use slower efficiency cores.",
                config.mining.threads, hw.cpu_cores_performance
            ),
            fix: format!(
                "Consider reducing to {} threads. E-cores add little hashrate but increase power and heat.",
                hw.cpu_cores_performance
            ),
        });
    }

    // GPU intensity not at max when GPU is enabled
    if config.gpu.enabled && config.gpu.intensity < 0.9 {
        warnings.push(OptimizationWarning {
            severity: Severity::Low,
            message: format!(
                "GPU intensity is {:.0}%. Not running at full capacity.",
                config.gpu.intensity * 100.0
            ),
            fix: "Set GPU intensity to 100% in Settings for maximum hashrate.".to_string(),
        });
    }

    // Activity throttling will reduce hashrate when user is active
    if config.activity.enabled && config.activity.min_threads < hw.cpu_cores_performance {
        warnings.push(OptimizationWarning {
            severity: Severity::Info,
            message: format!(
                "Activity throttling is on (idle timeout: {}s). Mining drops to {} thread(s) when you're active.",
                config.activity.idle_timeout_secs, config.activity.min_threads
            ),
            fix: format!(
                "THROTTLE_COUNTDOWN:{}",
                config.activity.idle_timeout_secs
            ),
        });
    }

    warnings
}

#[derive(Debug, Clone, Serialize)]
pub struct OptimizationWarning {
    pub severity: Severity,
    pub message: String,
    pub fix: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
    Info,
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

    #[test]
    fn test_check_optimization_with_default_config() {
        let config = crate::config::MinerConfig::default();
        let warnings = check_optimization(&config);

        // Default config has activity throttling enabled, so we should get
        // at least the Info-level throttling warning.
        let has_activity_info = warnings.iter().any(|w| {
            matches!(w.severity, Severity::Info)
                && w.message.contains("Activity throttling")
        });
        assert!(
            has_activity_info,
            "Expected activity throttling info warning with default config. Got: {:?}",
            warnings
        );
    }

    #[test]
    fn test_check_optimization_gpu_enabled_optimal() {
        let hw = detect();
        let mut config = crate::config::MinerConfig::default();
        config.gpu.enabled = true;
        config.gpu.intensity = 1.0;
        config.mining.threads = hw.cpu_cores_performance;
        config.mining.gpu_only = false;

        let warnings = check_optimization(&config);

        // Should NOT have the "GPU disabled" warning
        let has_gpu_disabled = warnings.iter().any(|w| {
            w.message.contains("GPU mining is disabled")
        });
        assert!(
            !has_gpu_disabled,
            "Should not warn about GPU being disabled when it is enabled"
        );

        // Should NOT have the "fewer threads than P-cores" warning
        let has_fewer_threads = warnings.iter().any(|w| {
            matches!(w.severity, Severity::Medium) && w.message.contains("CPU threads")
        });
        assert!(
            !has_fewer_threads,
            "Should not warn about thread count when using all P-cores"
        );

        // Should NOT have the GPU intensity warning (intensity is 1.0 >= 0.9)
        let has_intensity_warning = warnings.iter().any(|w| {
            w.message.contains("GPU intensity")
        });
        assert!(
            !has_intensity_warning,
            "Should not warn about GPU intensity when at 100%"
        );
    }

    #[test]
    fn test_check_optimization_gpu_disabled() {
        let hw = detect();
        let mut config = crate::config::MinerConfig::default();
        config.gpu.enabled = false;
        config.mining.threads = hw.cpu_cores_performance;

        let warnings = check_optimization(&config);

        if hw.gpu_available {
            // If the test machine has a GPU, we should get a High severity warning
            let has_gpu_warning = warnings.iter().any(|w| {
                matches!(w.severity, Severity::High)
                    && w.message.contains("GPU mining is disabled")
            });
            assert!(
                has_gpu_warning,
                "Expected GPU disabled warning when GPU is available but disabled"
            );
        }
        // If no GPU on the test machine, no GPU-related warning is expected
    }

    #[test]
    fn test_check_optimization_low_gpu_intensity() {
        let mut config = crate::config::MinerConfig::default();
        config.gpu.enabled = true;
        config.gpu.intensity = 0.5;

        let warnings = check_optimization(&config);

        let has_intensity_warning = warnings.iter().any(|w| {
            matches!(w.severity, Severity::Low)
                && w.message.contains("GPU intensity")
        });
        assert!(
            has_intensity_warning,
            "Expected low GPU intensity warning when intensity is 50%"
        );
    }

    #[test]
    fn test_check_optimization_fewer_threads_than_pcores() {
        let hw = detect();
        if hw.cpu_cores_performance <= 1 {
            // Can't test this on single-core machines
            return;
        }
        let mut config = crate::config::MinerConfig::default();
        config.mining.threads = 1;
        config.mining.gpu_only = false;

        let warnings = check_optimization(&config);

        let has_thread_warning = warnings.iter().any(|w| {
            matches!(w.severity, Severity::Medium)
                && w.message.contains("CPU threads")
                && w.message.contains("performance cores")
        });
        assert!(
            has_thread_warning,
            "Expected thread count warning when using 1 thread with {} P-cores. Got: {:?}",
            hw.cpu_cores_performance, warnings
        );
    }

    #[test]
    fn test_check_optimization_too_many_threads() {
        let hw = detect();
        if hw.cpu_cores_performance >= hw.cpu_cores_total {
            // All cores are P-cores; can't trigger the E-core warning
            return;
        }
        let mut config = crate::config::MinerConfig::default();
        config.mining.threads = hw.cpu_cores_total; // includes E-cores

        let warnings = check_optimization(&config);

        let has_ecore_warning = warnings.iter().any(|w| {
            matches!(w.severity, Severity::Low)
                && w.message.contains("efficiency cores")
        });
        assert!(
            has_ecore_warning,
            "Expected E-core warning when using {} threads with {} P-cores. Got: {:?}",
            hw.cpu_cores_total, hw.cpu_cores_performance, warnings
        );
    }

    #[test]
    fn test_optimization_warning_serializes() {
        let warning = OptimizationWarning {
            severity: Severity::High,
            message: "Test message".to_string(),
            fix: "Test fix".to_string(),
        };
        let json = serde_json::to_string(&warning).unwrap();
        assert!(json.contains("\"severity\":\"high\""));
        assert!(json.contains("\"message\":\"Test message\""));
        assert!(json.contains("\"fix\":\"Test fix\""));
    }

    #[test]
    fn test_severity_serializes_lowercase() {
        let high = serde_json::to_string(&Severity::High).unwrap();
        let medium = serde_json::to_string(&Severity::Medium).unwrap();
        let low = serde_json::to_string(&Severity::Low).unwrap();
        let info = serde_json::to_string(&Severity::Info).unwrap();

        assert_eq!(high, "\"high\"");
        assert_eq!(medium, "\"medium\"");
        assert_eq!(low, "\"low\"");
        assert_eq!(info, "\"info\"");
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
