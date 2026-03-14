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
