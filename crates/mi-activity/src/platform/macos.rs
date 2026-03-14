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
