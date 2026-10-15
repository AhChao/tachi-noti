//! The GUI-subsystem exe starts without a console, so CLI subcommands run
//! from a terminal would print into the void. Borrow the parent's console.

use std::os::windows::io::IntoRawHandle;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
};

/// Attach to the parent's console (if any) and point stdout / stderr at it
/// unless they were already redirected to a file or pipe by the caller.
pub fn attach_parent() {
    // SAFETY: plain Win32 calls; failure (no parent console, e.g. launched
    // from Explorer) just means there is nothing to print to.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        let mut console: Option<HANDLE> = None;
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if has_handle(which) {
                continue;
            }
            // One CONOUT$ handle serves both streams; it lives for the whole
            // process on purpose.
            if console.is_none() {
                console = std::fs::OpenOptions::new()
                    .write(true)
                    .open("CONOUT$")
                    .ok()
                    .map(|f| HANDLE(f.into_raw_handle()));
            }
            if let Some(h) = console {
                let _ = SetStdHandle(which, h);
            }
        }
    }
}

/// SAFETY: caller is single-threaded startup code; GetStdHandle has no preconditions.
unsafe fn has_handle(which: STD_HANDLE) -> bool {
    unsafe { GetStdHandle(which) }.map(|h| !h.is_invalid()).unwrap_or(false)
}
