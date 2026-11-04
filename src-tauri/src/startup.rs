//! Manual activation always wins over a saved background-start preference.
use std::sync::atomic::{AtomicBool, Ordering};

static BACKGROUND: AtomicBool = AtomicBool::new(false);
static FOREGROUND_REQUESTED: AtomicBool = AtomicBool::new(false);

pub(crate) fn capture_launch() {
    let background = std::env::args()
        .skip(1)
        .any(|arg| matches!(arg.as_str(), "--background" | "--hidden" | "--minimized"));
    #[cfg(target_os = "macos")]
    let background = background || macos_login_launch();
    BACKGROUND.store(background, Ordering::Relaxed);
}

pub(crate) fn request_foreground() {
    FOREGROUND_REQUESTED.store(true, Ordering::Relaxed);
}

fn hide_at_launch(osir: bool, silent: bool, background: bool, foreground: bool) -> bool {
    !foreground && silent && (!osir || background)
}

pub(crate) fn should_hide() -> bool {
    hide_at_launch(
        crate::distribution::is_osir(),
        crate::settings::get_settings().silent_startup,
        BACKGROUND.load(Ordering::Relaxed),
        FOREGROUND_REQUESTED.load(Ordering::Relaxed),
    )
}

#[cfg(target_os = "macos")]
fn macos_login_launch() -> bool {
    use objc2_06::{
        msg_send,
        runtime::{AnyClass, AnyObject},
    };
    // Apple's kAEOpenApplication carries keyAEPropData ('prdt') when
    // Launch Services starts a login/service item, including hidden login items.
    // Called on the AppKit thread during setup, before displaying any window.
    unsafe {
        let Some(class) = AnyClass::get(c"NSAppleEventManager") else {
            return false;
        };
        let manager: *mut AnyObject = msg_send![class, sharedAppleEventManager];
        let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
        if event.is_null() {
            return false;
        }
        let property: *mut AnyObject =
            msg_send![event, paramDescriptorForKeyword: u32::from_be_bytes(*b"prdt")];
        if property.is_null() {
            return false;
        }
        let kind: u32 = msg_send![property, enumCodeValue];
        kind == u32::from_be_bytes(*b"lgit") || kind == u32::from_be_bytes(*b"svit")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn manual_osir_launch_and_reopen_show_even_with_silent_preference() {
        assert!(!hide_at_launch(true, true, false, false));
        assert!(!hide_at_launch(true, true, true, true));
        assert!(hide_at_launch(true, true, true, false));
        assert!(!hide_at_launch(true, false, true, false));
        assert!(hide_at_launch(false, true, false, false));
    }
}
