use std::ffi::CString;
use std::marker::{PhantomData, PhantomPinned};

type CFStringRef = *const std::ffi::c_void;
type CFAllocatorRef = *const std::ffi::c_void;
type CFDictionaryRef = *const std::ffi::c_void;
type CFMutableDictionaryRef = *mut std::ffi::c_void;
type CFArrayRef = *const std::ffi::c_void;

#[repr(C)]
struct IOReportSubscription {
    _data: [u8; 0],
    _phantom: PhantomData<(*mut u8, PhantomPinned)>,
}
type IOReportSubscriptionRef = *const IOReportSubscription;

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_IOPM_ASSERTION_LEVEL_ON: u32 = 255;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const std::os::raw::c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: *const std::ffi::c_void);
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut std::os::raw::c_char,
        buffer_size: i64,
        encoding: u32,
    ) -> u8;
    fn CFArrayGetCount(array: CFArrayRef) -> i64;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: i64) -> *const std::ffi::c_void;
    fn CFDictionaryGetValue(
        dict: CFDictionaryRef,
        key: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
}

#[link(name = "IOReport", kind = "dylib")]
extern "C" {
    fn IOReportCopyChannelsInGroup(
        group: CFStringRef,
        subgroup: CFStringRef,
        c: u64,
        d: u64,
        e: u64,
    ) -> CFDictionaryRef;
    fn IOReportCreateSubscription(
        a: *const std::ffi::c_void,
        channels: CFMutableDictionaryRef,
        sub_channels: *mut CFMutableDictionaryRef,
        d: u64,
        e: *const std::ffi::c_void,
    ) -> IOReportSubscriptionRef;
    fn IOReportCreateSamples(
        subscription: IOReportSubscriptionRef,
        channels: CFMutableDictionaryRef,
        a: *const std::ffi::c_void,
    ) -> CFDictionaryRef;
    fn IOReportCreateSamplesDelta(
        prev: CFDictionaryRef,
        current: CFDictionaryRef,
        a: *const std::ffi::c_void,
    ) -> CFDictionaryRef;
    fn IOReportChannelGetChannelName(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportChannelGetUnitLabel(channel: CFDictionaryRef) -> CFStringRef;
    fn IOReportSimpleGetIntegerValue(channel: CFDictionaryRef, a: i32) -> i64;
}

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        level: u32,
        reason_for_activity: CFStringRef,
        assertion_id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(assertion_id: u32) -> i32;
}

fn cfstring(s: &str) -> CFStringRef {
    let cstr = CString::new(s).unwrap();
    unsafe { CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), K_CF_STRING_ENCODING_UTF8) }
}

/// Holds a macOS IOPMAssertion that prevents user-idle system sleep.
/// The display is allowed to turn off, but the CPU/GPU stay running.
pub struct SleepInhibitor {
    assertion_id: Option<u32>,
}

impl SleepInhibitor {
    pub fn new() -> Self {
        Self { assertion_id: None }
    }

    /// Acquire the power assertion (prevents idle sleep). No-op if already held.
    pub fn enable(&mut self) {
        if self.assertion_id.is_some() {
            return;
        }
        unsafe {
            let assertion_type = cfstring("PreventUserIdleSystemSleep");
            let reason = cfstring("mi-miner: mining is active");
            let mut assertion_id: u32 = 0;

            let result = IOPMAssertionCreateWithName(
                assertion_type,
                K_IOPM_ASSERTION_LEVEL_ON,
                reason,
                &mut assertion_id,
            );

            CFRelease(assertion_type);
            CFRelease(reason);

            if result == 0 {
                tracing::info!("Sleep inhibitor: acquired (system will stay awake while mining)");
                self.assertion_id = Some(assertion_id);
            } else {
                tracing::warn!("Sleep inhibitor: failed to create assertion (error {result})");
            }
        }
    }

    /// Release the power assertion (allow idle sleep again). No-op if not held.
    pub fn disable(&mut self) {
        if let Some(id) = self.assertion_id.take() {
            unsafe {
                IOPMAssertionRelease(id);
            }
            tracing::info!("Sleep inhibitor: released (system may sleep normally)");
        }
    }

    pub fn is_active(&self) -> bool {
        self.assertion_id.is_some()
    }
}

impl Drop for SleepInhibitor {
    fn drop(&mut self) {
        self.disable();
    }
}

/// Get seconds since last user input event on macOS.
/// Uses CGEventSourceSecondsSinceLastEventType via FFI.
/// This doesn't require accessibility permissions.
pub fn idle_seconds() -> f64 {
    use std::os::raw::c_uint;

    // CGEventSourceStateID::CombinedSessionState = 0
    const COMBINED_SESSION_STATE: i32 = 0;

    // CGEventType values
    const KEY_DOWN: c_uint = 10;
    const MOUSE_MOVED: c_uint = 5;
    const LEFT_MOUSE_DOWN: c_uint = 1;

    extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(
            stateID: i32,
            eventType: c_uint,
        ) -> f64;
    }

    unsafe {
        let keyboard_idle =
            CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, KEY_DOWN);
        let mouse_idle =
            CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, MOUSE_MOVED);
        let click_idle =
            CGEventSourceSecondsSinceLastEventType(COMBINED_SESSION_STATE, LEFT_MOUSE_DOWN);

        keyboard_idle.min(mouse_idle).min(click_idle)
    }
}

/// Power consumption reading from IOReport (in milliwatts).
#[derive(Debug, Clone, Default)]
pub struct PowerReading {
    pub cpu_mw: u64,
    pub gpu_mw: u64,
    pub ane_mw: u64,
    pub dram_mw: u64,
    pub total_mw: u64,
}

/// Reads system power consumption via the IOReport private framework.
/// Does not require sudo. Works on Apple Silicon only.
pub struct PowerSampler {
    subscription: IOReportSubscriptionRef,
    channels: CFMutableDictionaryRef,
    prev_sample: CFDictionaryRef,
}

unsafe fn cfstring_to_string(cf: CFStringRef) -> Option<String> {
    if cf.is_null() {
        return None;
    }
    let mut buf = [0i8; 256];
    let ok = CFStringGetCString(cf, buf.as_mut_ptr(), 256, K_CF_STRING_ENCODING_UTF8);
    if ok == 0 {
        return None;
    }
    Some(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
}

/// Convert IOReport energy value to milliwatts given elapsed time and unit label.
unsafe fn ior_to_milliwatts(item: CFDictionaryRef, unit: &str, duration_ms: u64) -> u64 {
    let val = IOReportSimpleGetIntegerValue(item, 0);
    if val <= 0 || duration_ms == 0 {
        return 0;
    }
    // val is in the unit indicated by unit label; convert to millijoules first
    let mj = match unit {
        "mJ" => val as f64,
        "uJ" => val as f64 / 1e3,
        "nJ" => val as f64 / 1e6,
        _ => return 0,
    };
    // power (mW) = energy (mJ) / time (s)
    let mw = mj / (duration_ms as f64 / 1000.0);
    mw.max(0.0) as u64
}

impl PowerSampler {
    /// Create a new power sampler. Returns None if IOReport is unavailable.
    pub fn new() -> Option<Self> {
        unsafe {
            let group = cfstring("Energy Model");
            let channels = IOReportCopyChannelsInGroup(group, std::ptr::null(), 0, 0, 0);
            CFRelease(group);

            if channels.is_null() {
                tracing::debug!("IOReport: Energy Model channels not available");
                return None;
            }

            let mut sub_channels: CFMutableDictionaryRef = std::ptr::null_mut();
            let subscription = IOReportCreateSubscription(
                std::ptr::null(),
                channels as CFMutableDictionaryRef,
                &mut sub_channels,
                0,
                std::ptr::null(),
            );

            if subscription.is_null() {
                CFRelease(channels);
                tracing::debug!("IOReport: failed to create subscription");
                return None;
            }

            // Take initial sample
            let initial = IOReportCreateSamples(subscription, sub_channels, std::ptr::null());
            if initial.is_null() {
                tracing::debug!("IOReport: failed to take initial sample");
                return None;
            }

            tracing::info!("Power monitoring: active (IOReport)");

            Some(Self {
                subscription,
                channels: sub_channels,
                prev_sample: initial,
            })
        }
    }

    /// Take a new sample and return power readings since the last call.
    /// `elapsed_ms` is the time since the last call in milliseconds.
    pub fn sample(&mut self, elapsed_ms: u64) -> Option<PowerReading> {
        unsafe {
            let current = IOReportCreateSamples(
                self.subscription,
                self.channels,
                std::ptr::null(),
            );
            if current.is_null() {
                return None;
            }

            let delta = IOReportCreateSamplesDelta(
                self.prev_sample,
                current,
                std::ptr::null(),
            );

            CFRelease(self.prev_sample);
            self.prev_sample = current;

            if delta.is_null() {
                return None;
            }

            let reading = extract_power(delta, elapsed_ms);
            CFRelease(delta);
            Some(reading)
        }
    }
}

// SAFETY: IOReport handles are not thread-safe but the PowerSampler is only
// used from a single async task (the activity monitor loop). The raw pointers
// need Send to cross the tokio::spawn boundary into that task.
unsafe impl Send for PowerSampler {}

impl Drop for PowerSampler {
    fn drop(&mut self) {
        if !self.prev_sample.is_null() {
            unsafe { CFRelease(self.prev_sample) };
        }
    }
}

unsafe fn extract_power(delta: CFDictionaryRef, elapsed_ms: u64) -> PowerReading {
    let mut reading = PowerReading::default();

    let key = cfstring("IOReportChannels");
    let channels_array = CFDictionaryGetValue(delta, key);
    CFRelease(key);

    if channels_array.is_null() {
        return reading;
    }

    let count = CFArrayGetCount(channels_array);
    for i in 0..count {
        let item = CFArrayGetValueAtIndex(channels_array, i);
        if item.is_null() {
            continue;
        }

        let name_cf = IOReportChannelGetChannelName(item);
        let unit_cf = IOReportChannelGetUnitLabel(item);

        let name = match cfstring_to_string(name_cf) {
            Some(n) => n,
            None => continue,
        };
        let unit = cfstring_to_string(unit_cf).unwrap_or_default();

        let mw = ior_to_milliwatts(item, &unit, elapsed_ms);

        if name == "GPU Energy" {
            reading.gpu_mw += mw;
        } else if name.ends_with("CPU Energy") {
            reading.cpu_mw += mw;
        } else if name.starts_with("ANE") {
            reading.ane_mw += mw;
        } else if name.starts_with("DRAM") {
            reading.dram_mw += mw;
        }
    }

    reading.total_mw = reading.cpu_mw + reading.gpu_mw + reading.ane_mw + reading.dram_mw;
    reading
}
