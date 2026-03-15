#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub mod linux;

/// Get seconds since last user input (keyboard/mouse).
pub fn idle_seconds() -> f64 {
    #[cfg(target_os = "macos")]
    {
        return macos::idle_seconds();
    }

    #[cfg(target_os = "linux")]
    {
        return linux::idle_seconds();
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        return 0.0; // Always "active" on unsupported platforms
    }
}

/// Create a power sampler if available on this platform.
/// Returns None on unsupported platforms or if IOReport is unavailable.
pub fn power_sampler() -> Option<PowerSampler> {
    #[cfg(target_os = "macos")]
    {
        return macos::PowerSampler::new().map(|inner| PowerSampler { inner });
    }

    #[cfg(not(target_os = "macos"))]
    {
        return None;
    }
}

/// System power consumption reading (milliwatts).
#[derive(Debug, Clone, Default)]
pub struct PowerReading {
    pub cpu_mw: u64,
    pub gpu_mw: u64,
    pub ane_mw: u64,
    pub dram_mw: u64,
    pub total_mw: u64,
}

#[cfg(target_os = "macos")]
impl From<macos::PowerReading> for PowerReading {
    fn from(r: macos::PowerReading) -> Self {
        Self {
            cpu_mw: r.cpu_mw,
            gpu_mw: r.gpu_mw,
            ane_mw: r.ane_mw,
            dram_mw: r.dram_mw,
            total_mw: r.total_mw,
        }
    }
}

/// Wrapper for platform-specific power sampler.
pub struct PowerSampler {
    #[cfg(target_os = "macos")]
    inner: macos::PowerSampler,
}

// SAFETY: The inner platform sampler is only used from a single async task.
unsafe impl Send for PowerSampler {}

impl PowerSampler {
    pub fn sample(&mut self, elapsed_ms: u64) -> Option<PowerReading> {
        #[cfg(target_os = "macos")]
        {
            return self.inner.sample(elapsed_ms).map(PowerReading::from);
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = elapsed_ms;
            return None;
        }
    }
}

/// Prevents user-idle system sleep while held. Display may turn off,
/// but the CPU/GPU keep running so mining continues.
///
/// On non-macOS platforms this is a no-op.
pub struct SleepInhibitor {
    #[cfg(target_os = "macos")]
    inner: macos::SleepInhibitor,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            inner: macos::SleepInhibitor::new(),
        }
    }

    pub fn enable(&mut self) {
        #[cfg(target_os = "macos")]
        self.inner.enable();
    }

    pub fn disable(&mut self) {
        #[cfg(target_os = "macos")]
        self.inner.disable();
    }

    #[allow(dead_code)]
    pub fn is_active(&self) -> bool {
        #[cfg(target_os = "macos")]
        { return self.inner.is_active(); }
        #[cfg(not(target_os = "macos"))]
        { false }
    }
}
