use crate::config::{ActivityConfig, MinerConfig};
use std::sync::{Arc, RwLock};

/// Runtime-mutable configuration. Subsystems read from this;
/// the web dashboard writes to it when the user saves settings.
#[derive(Debug)]
pub struct LiveConfig {
    inner: RwLock<MinerConfig>,
}

impl LiveConfig {
    pub fn new(config: MinerConfig) -> Arc<Self> {
        Arc::new(Self {
            inner: RwLock::new(config),
        })
    }

    /// Update the live config and save to disk.
    pub fn update(&self, config: MinerConfig) -> Result<(), crate::MiMinerError> {
        let path = MinerConfig::default_path();
        config.save(&path)?;
        *self.inner.write().unwrap() = config;
        tracing::info!("Live config updated");
        Ok(())
    }

    /// Read the current activity config.
    pub fn activity(&self) -> ActivityConfig {
        self.inner.read().unwrap().activity.clone()
    }

    /// Read the current mining thread count.
    pub fn mining_threads(&self) -> usize {
        self.inner.read().unwrap().mining.threads
    }

    /// Read GPU intensity.
    pub fn gpu_intensity(&self) -> f32 {
        self.inner.read().unwrap().gpu.intensity
    }

    /// Get a full snapshot of the config.
    pub fn snapshot(&self) -> MinerConfig {
        self.inner.read().unwrap().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_creates_from_config() {
        let config = MinerConfig::default();
        let live = LiveConfig::new(config.clone());
        // Should be wrapped in Arc
        let snap = live.snapshot();
        assert_eq!(snap.mining.threads, config.mining.threads);
        assert_eq!(snap.gpu.enabled, config.gpu.enabled);
        assert_eq!(snap.stratum.url, config.stratum.url);
    }

    #[test]
    fn test_snapshot_returns_full_config() {
        let mut config = MinerConfig::default();
        config.mining.threads = 42;
        config.gpu.intensity = 0.75;
        config.stratum.worker = "test_worker".to_string();
        config.activity.idle_timeout_secs = 999;

        let live = LiveConfig::new(config);
        let snap = live.snapshot();

        assert_eq!(snap.mining.threads, 42);
        assert_eq!(snap.gpu.intensity, 0.75);
        assert_eq!(snap.stratum.worker, "test_worker");
        assert_eq!(snap.activity.idle_timeout_secs, 999);
    }

    #[test]
    fn test_activity_returns_activity_config() {
        let mut config = MinerConfig::default();
        config.activity.enabled = false;
        config.activity.idle_timeout_secs = 300;
        config.activity.min_threads = 4;
        config.activity.min_gpu_intensity = 0.5;

        let live = LiveConfig::new(config);
        let activity = live.activity();

        assert!(!activity.enabled);
        assert_eq!(activity.idle_timeout_secs, 300);
        assert_eq!(activity.min_threads, 4);
        assert_eq!(activity.min_gpu_intensity, 0.5);
    }

    #[test]
    fn test_mining_threads_returns_thread_count() {
        let mut config = MinerConfig::default();
        config.mining.threads = 16;

        let live = LiveConfig::new(config);
        assert_eq!(live.mining_threads(), 16);
    }

    #[test]
    fn test_gpu_intensity_returns_intensity() {
        let mut config = MinerConfig::default();
        config.gpu.intensity = 0.42;

        let live = LiveConfig::new(config);
        assert!((live.gpu_intensity() - 0.42).abs() < f32::EPSILON);
    }

    // NOTE: Tests that call update() need HOME override (for config save to disk).
    // Combined into one test because HOME is process-global and races with parallel tests.
    #[test]
    fn test_update_and_disk_persistence() {
        let test_dir = std::env::temp_dir().join("mi-miner-test-live-config-combined");
        let _ = std::fs::create_dir_all(&test_dir);
        let _ = std::fs::remove_dir_all(&test_dir.join(".mi-miner"));

        let orig_home = std::env::var("HOME").ok();
        std::env::set_var("HOME", test_dir.to_str().unwrap());

        // --- Part 1: update() modifies live config and saves to disk ---
        let config = MinerConfig::default();
        let live = LiveConfig::new(config);

        let mut new_config = MinerConfig::default();
        new_config.mining.threads = 99;
        new_config.gpu.intensity = 0.33;
        new_config.stratum.worker = "updated_worker".to_string();

        let result = live.update(new_config);
        assert!(result.is_ok());

        // Verify the live config was updated in memory
        assert_eq!(live.mining_threads(), 99);
        assert!((live.gpu_intensity() - 0.33).abs() < f32::EPSILON);
        let snap = live.snapshot();
        assert_eq!(snap.stratum.worker, "updated_worker");

        // Verify it was saved to disk
        let path = MinerConfig::default_path();
        assert!(path.exists());
        let loaded = MinerConfig::load(&path).unwrap();
        assert_eq!(loaded.mining.threads, 99);

        // --- Part 2: multiple updates, snapshot reflects the latest ---
        let mut c1 = MinerConfig::default();
        c1.mining.threads = 10;
        live.update(c1).unwrap();
        assert_eq!(live.snapshot().mining.threads, 10);

        let mut c2 = MinerConfig::default();
        c2.mining.threads = 20;
        live.update(c2).unwrap();
        assert_eq!(live.snapshot().mining.threads, 20);

        // Cleanup
        let _ = std::fs::remove_dir_all(&test_dir.join(".mi-miner"));
        if let Some(h) = orig_home {
            std::env::set_var("HOME", h);
        }
    }

    #[test]
    fn test_concurrent_reads() {
        let mut config = MinerConfig::default();
        config.mining.threads = 8;
        config.gpu.intensity = 0.5;
        let live = LiveConfig::new(config);

        // Multiple concurrent readers should all see the same values
        let mut handles = vec![];
        for _ in 0..4 {
            let l = live.clone();
            handles.push(std::thread::spawn(move || {
                let threads = l.mining_threads();
                let intensity = l.gpu_intensity();
                let snap = l.snapshot();
                (threads, intensity, snap.mining.threads)
            }));
        }

        for h in handles {
            let (threads, intensity, snap_threads) = h.join().unwrap();
            assert_eq!(threads, 8);
            assert!((intensity - 0.5).abs() < f32::EPSILON);
            assert_eq!(snap_threads, 8);
        }
    }
}
