use mi_core::config::ActivityConfig;

/// Throttle decision: how many CPU threads and what GPU intensity to use.
#[derive(Debug, Clone)]
pub struct ThrottleState {
    pub target_threads: usize,
    pub target_gpu_intensity: f32,
    pub is_ramping: bool,
}

/// Compute the desired throttle state based on idle time and CPU usage.
pub fn compute_throttle(
    config: &ActivityConfig,
    idle_secs: f64,
    external_cpu_pct: f32,
    max_threads: usize,
) -> ThrottleState {
    let user_active = idle_secs < config.idle_timeout_secs as f64;

    if user_active {
        // User is active: use minimum resources
        ThrottleState {
            target_threads: config.min_threads.max(1),
            target_gpu_intensity: config.min_gpu_intensity,
            is_ramping: false,
        }
    } else if external_cpu_pct > config.cpu_threshold {
        // User idle but system is busy (e.g., compiling, rendering)
        ThrottleState {
            target_threads: (max_threads / 2).max(config.min_threads).max(1),
            target_gpu_intensity: 0.5,
            is_ramping: false,
        }
    } else {
        // User idle and system is idle: full power
        ThrottleState {
            target_threads: max_threads,
            target_gpu_intensity: 1.0,
            is_ramping: true,
        }
    }
}

/// Smoothly ramp between current and target values.
pub fn ramp_value(current: f32, target: f32, ramp_secs: f64, elapsed_secs: f64) -> f32 {
    if ramp_secs <= 0.0 || elapsed_secs >= ramp_secs {
        return target;
    }

    let progress = (elapsed_secs / ramp_secs) as f32;
    current + (target - current) * progress.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> ActivityConfig {
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

    #[test]
    fn test_user_active_uses_minimum() {
        let config = test_config();
        let state = compute_throttle(&config, 10.0, 0.0, 12); // 10s idle, below 120s timeout
        assert_eq!(state.target_threads, 1); // min_threads
        assert_eq!(state.target_gpu_intensity, 0.1); // min_gpu_intensity
        assert!(!state.is_ramping);
    }

    #[test]
    fn test_user_idle_system_idle_full_power() {
        let config = test_config();
        let state = compute_throttle(&config, 200.0, 10.0, 12); // 200s idle, low CPU
        assert_eq!(state.target_threads, 12); // max threads
        assert_eq!(state.target_gpu_intensity, 1.0);
        assert!(state.is_ramping);
    }

    #[test]
    fn test_user_idle_system_busy_half_power() {
        let config = test_config();
        let state = compute_throttle(&config, 200.0, 80.0, 12); // idle but CPU high
        assert_eq!(state.target_threads, 6); // 12/2
        assert_eq!(state.target_gpu_intensity, 0.5);
        assert!(!state.is_ramping);
    }

    #[test]
    fn test_boundary_exactly_at_timeout() {
        let config = test_config();
        // Exactly at timeout boundary — should still be active
        let state = compute_throttle(&config, 119.9, 0.0, 12);
        assert_eq!(state.target_threads, 1);
        // Just past timeout — idle
        let state = compute_throttle(&config, 120.0, 0.0, 12);
        assert_eq!(state.target_threads, 12);
    }

    #[test]
    fn test_min_threads_at_least_one() {
        let mut config = test_config();
        config.min_threads = 0; // user sets 0
        let state = compute_throttle(&config, 10.0, 0.0, 12);
        assert!(state.target_threads >= 1); // should clamp to 1
    }

    #[test]
    fn test_system_busy_with_low_max_threads() {
        let config = test_config();
        let state = compute_throttle(&config, 200.0, 80.0, 2); // only 2 max threads
        assert_eq!(state.target_threads, 1); // 2/2 = 1, max with min_threads
    }

    #[test]
    fn test_ramp_value_at_start() {
        let val = ramp_value(0.0, 1.0, 10.0, 0.0);
        assert_eq!(val, 0.0);
    }

    #[test]
    fn test_ramp_value_at_end() {
        let val = ramp_value(0.0, 1.0, 10.0, 10.0);
        assert_eq!(val, 1.0);
    }

    #[test]
    fn test_ramp_value_past_end() {
        let val = ramp_value(0.0, 1.0, 10.0, 20.0);
        assert_eq!(val, 1.0);
    }

    #[test]
    fn test_ramp_value_midpoint() {
        let val = ramp_value(0.0, 1.0, 10.0, 5.0);
        assert!((val - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_ramp_value_zero_ramp_secs() {
        let val = ramp_value(0.0, 1.0, 0.0, 0.0);
        assert_eq!(val, 1.0); // instant
    }

    #[test]
    fn test_ramp_value_decreasing() {
        let val = ramp_value(1.0, 0.0, 10.0, 5.0);
        assert!((val - 0.5).abs() < 0.01);
    }
}
