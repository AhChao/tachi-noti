//! Windows implementation of the platform contract (see `platform`).
//!
//! Toasts go through WinRT (`Windows.UI.Notifications`) under our own
//! AppUserModelID, registered per user by `register::install` (no admin, no
//! MSIX package). A toast click launches the `tachi-noti://` URL protocol,
//! which runs `tachi-noti activate <uri>` → `host::focus_host`.

pub mod host;
pub mod notify;
pub mod proc;
pub mod register;
pub mod shell;
pub mod sound;

/// AppUserModelID our toasts are attributed to. It must match the Start menu
/// shortcut's System.AppUserModel.ID and the registry identity exactly.
pub const AUMID: &str = "TachiNoti.Tachi";

/// Display name Windows shows on the toast and in Settings → Notifications.
pub const APP_DISPLAY_NAME: &str = "Tachi";
