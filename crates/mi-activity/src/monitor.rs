use crate::platform;
use crate::throttle::{compute_throttle, ThrottleState};
use mi_core::LiveConfig;
use mi_core::MiningStats;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use sysinfo::System;
use tokio::sync::watch;

/// Activity monitor that polls user input and system CPU, then broadcasts throttle decisions.
pub struct ActivityMonitor {
    live_config: Arc<LiveConfig>,
    stats: Arc<MiningStats>,
    max_threads: usize,
}

impl ActivityMonitor {
    pub fn new(live_config: Arc<LiveConfig>, stats: Arc<MiningStats>, max_threads: usize) -> Self {
        Self {
            live_config,
            stats,
            max_threads,
        }
    }

    /// Run the activity monitor loop. Sends throttle decisions on the watch channel.
    pub async fn run(
        self,
        throttle_tx: watch::Sender<ThrottleState>,
    ) -> Result<(), mi_core::MiMinerError> {
        let mut sys = System::new();
        let mut power_sampler = platform::power_sampler();

        #[cfg(target_os = "linux")]
        {
            crate::platform::linux::start_input_monitor();
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            interval.tick().await;

            if self.stats.should_stop.load(Ordering::Relaxed) {
                break;
            }

            // Read current activity config (may have been updated via dashboard)
            let config = self.live_config.activity();

            let idle_secs = platform::idle_seconds();
            self.stats
                .idle_secs
                .store(idle_secs as u64, Ordering::Relaxed);

            let user_active = idle_secs < config.idle_timeout_secs as f64;
            self.stats.is_user_active.store(user_active, Ordering::Relaxed);

            sys.refresh_cpu_usage();
            let cpu_usage: f32 = sys.cpus().iter().map(|c| c.cpu_usage()).sum::<f32>()
                / sys.cpus().len().max(1) as f32;

            let throttle =
                compute_throttle(&config, idle_secs, cpu_usage, self.max_threads);

            // Sample power consumption
            if let Some(ref mut sampler) = power_sampler {
                if let Some(power) = sampler.sample(1000) {
                    self.stats.power_cpu_mw.store(power.cpu_mw, Ordering::Relaxed);
                    self.stats.power_gpu_mw.store(power.gpu_mw, Ordering::Relaxed);
                    self.stats.power_ane_mw.store(power.ane_mw, Ordering::Relaxed);
                    self.stats.power_dram_mw.store(power.dram_mw, Ordering::Relaxed);
                    self.stats.power_total_mw.store(power.total_mw, Ordering::Relaxed);
                }
            }

            tracing::trace!(
                idle_secs = idle_secs as u64,
                user_active = user_active,
                cpu_pct = cpu_usage,
                target_threads = throttle.target_threads,
                target_gpu = throttle.target_gpu_intensity,
                "Activity check"
            );

            let _ = throttle_tx.send(throttle);
        }

        tracing::info!("Activity monitor stopped");
        Ok(())
    }
}
