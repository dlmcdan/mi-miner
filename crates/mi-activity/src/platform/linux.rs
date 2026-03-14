use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

static LAST_EVENT_MS: AtomicU64 = AtomicU64::new(0);
static INIT_TIME: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Get seconds since last user input event on Linux.
/// Falls back to 0 (always active) if evdev is not available.
pub fn idle_seconds() -> f64 {
    let init = INIT_TIME.get_or_init(Instant::now);
    let last = LAST_EVENT_MS.load(Ordering::Relaxed);
    if last == 0 {
        return 0.0; // No events seen yet, assume active
    }
    let now_ms = init.elapsed().as_millis() as u64;
    (now_ms.saturating_sub(last)) as f64 / 1000.0
}

/// Start the evdev listener in a background thread.
/// Call this once at startup on Linux.
pub fn start_input_monitor() {
    let init = INIT_TIME.get_or_init(Instant::now);

    std::thread::Builder::new()
        .name("input-monitor".to_string())
        .spawn(move || {
            if let Err(e) = monitor_input_devices() {
                tracing::warn!("Input monitoring failed: {e}");
            }
        })
        .ok();
}

fn monitor_input_devices() -> Result<(), Box<dyn std::error::Error>> {
    let init = INIT_TIME.get_or_init(Instant::now);
    let devices = evdev::enumerate()
        .filter(|(_, d)| {
            let types = d.supported_events();
            types.contains(evdev::EventType::KEY) || types.contains(evdev::EventType::RELATIVE)
        })
        .collect::<Vec<_>>();

    if devices.is_empty() {
        return Err("No input devices found (may need root)".into());
    }

    // Monitor all input devices (simplified: just check timestamps)
    for (path, device) in devices {
        let init = *init;
        std::thread::Builder::new()
            .name(format!("evdev-{}", path.display()))
            .spawn(move || {
                let mut device = device;
                loop {
                    match device.fetch_events() {
                        Ok(events) => {
                            for _event in events {
                                let now_ms = init.elapsed().as_millis() as u64;
                                LAST_EVENT_MS.store(now_ms, Ordering::Relaxed);
                            }
                        }
                        Err(e) => {
                            tracing::debug!("evdev read error: {e}");
                            break;
                        }
                    }
                }
            })
            .ok();
    }

    Ok(())
}
