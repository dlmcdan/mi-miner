use mi_core::MiningStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;

#[cfg(target_os = "macos")]
use crate::dispatcher::GpuDispatcher;
#[cfg(target_os = "macos")]
use crate::pipeline::MetalPipeline;

/// GPU mining manager. Wraps Metal pipeline on macOS, no-op on other platforms.
pub struct GpuManager {
    #[cfg(target_os = "macos")]
    dispatcher: Option<GpuDispatcher>,
    stats: Arc<MiningStats>,
    intensity: f32,
    available: bool,
}

impl GpuManager {
    /// Detect GPU and create manager.
    pub fn new(stats: Arc<MiningStats>, batch_size_log2: u32) -> Self {
        #[cfg(target_os = "macos")]
        {
            match Self::init_metal(batch_size_log2) {
                Ok(dispatcher) => {
                    tracing::info!("Metal GPU mining initialized");
                    return Self {
                        dispatcher: Some(dispatcher),
                        stats,
                        intensity: 1.0,
                        available: true,
                    };
                }
                Err(e) => {
                    tracing::warn!("Metal GPU not available: {e}");
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        tracing::info!("GPU mining not supported on this platform");

        Self {
            #[cfg(target_os = "macos")]
            dispatcher: None,
            stats,
            intensity: 0.0,
            available: false,
        }
    }

    #[cfg(target_os = "macos")]
    fn init_metal(batch_size_log2: u32) -> Result<GpuDispatcher, String> {
        // Try to load embedded metallib first (compiled at build time)
        let metallib_bytes = option_env!("MI_METALLIB_PATH");

        let pipeline = if let Some(path) = metallib_bytes {
            let path = std::path::Path::new(path);
            if path.exists() {
                MetalPipeline::new(path)?
            } else {
                return Err(format!("Metallib not found at {}", path.display()));
            }
        } else {
            // Check for metallib in the same directory as the binary
            let exe_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()));

            if let Some(dir) = exe_dir {
                let metallib_path = dir.join("sha256d.metallib");
                if metallib_path.exists() {
                    MetalPipeline::new(&metallib_path)?
                } else {
                    return Err(
                        "Metal shader not compiled. Install Xcode for GPU mining support."
                            .to_string(),
                    );
                }
            } else {
                return Err("Could not determine executable directory".to_string());
            }
        };

        Ok(GpuDispatcher::new(pipeline, batch_size_log2))
    }

    /// Check if GPU mining is available.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Set GPU intensity (0.0 - 1.0).
    pub fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity.clamp(0.0, 1.0);
        self.stats
            .gpu_intensity_pct
            .store((self.intensity * 100.0) as u64, Ordering::Relaxed);
    }

    /// Get current intensity.
    pub fn intensity(&self) -> f32 {
        self.intensity
    }

    /// Mine a batch of nonces on the GPU.
    /// Returns Some((nonce, hash_bytes)) if a valid nonce is found.
    pub fn mine_batch(
        &self,
        midstate: &[u32; 8],
        tail: &[u32; 4],
        target: &[u32; 8],
        nonce_start: u32,
    ) -> (Option<(u32, [u8; 32])>, u64) {
        #[cfg(target_os = "macos")]
        {
            if let Some(ref dispatcher) = self.dispatcher {
                let batch_count = ((dispatcher.batch_size() as f32 * self.intensity) as u64).max(1024);

                let result = dispatcher.dispatch_batch(
                    midstate,
                    tail,
                    target,
                    nonce_start,
                    self.intensity,
                );

                self.stats.add_gpu_hashes(batch_count);

                let found = result.map(|(nonce, hash_words)| {
                    let mut hash_bytes = [0u8; 32];
                    for (i, word) in hash_words.iter().enumerate() {
                        hash_bytes[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
                    }
                    (nonce, hash_bytes)
                });

                return (found, batch_count);
            }
        }

        (None, 0)
    }

    /// Get the number of nonces per GPU batch.
    pub fn batch_size(&self) -> u64 {
        #[cfg(target_os = "macos")]
        if let Some(ref dispatcher) = self.dispatcher {
            return dispatcher.batch_size();
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_manager_creation() {
        let stats = MiningStats::new();
        let mgr = GpuManager::new(stats, 24);
        // On CI/without Xcode, GPU won't be available
        // But the manager should still construct without panicking
        let _ = mgr.is_available();
    }

    #[test]
    fn test_gpu_manager_set_intensity() {
        let stats = MiningStats::new();
        let mut mgr = GpuManager::new(stats.clone(), 24);

        mgr.set_intensity(0.5);
        assert_eq!(mgr.intensity(), 0.5);
        assert_eq!(stats.gpu_intensity_pct.load(Ordering::Relaxed), 50);

        mgr.set_intensity(1.0);
        assert_eq!(mgr.intensity(), 1.0);
        assert_eq!(stats.gpu_intensity_pct.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn test_gpu_manager_clamps_intensity() {
        let stats = MiningStats::new();
        let mut mgr = GpuManager::new(stats, 24);

        mgr.set_intensity(-1.0);
        assert_eq!(mgr.intensity(), 0.0);

        mgr.set_intensity(5.0);
        assert_eq!(mgr.intensity(), 1.0);
    }

    #[test]
    fn test_gpu_mine_batch_when_unavailable() {
        let stats = MiningStats::new();
        let mgr = GpuManager::new(stats, 24);
        if !mgr.is_available() {
            let midstate = [0u32; 8];
            let tail = [0u32; 4];
            let target = [0u32; 8];
            let (result, count) = mgr.mine_batch(&midstate, &tail, &target, 0);
            assert!(result.is_none());
            assert_eq!(count, 0);
        }
    }
}
