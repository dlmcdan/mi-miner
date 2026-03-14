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
