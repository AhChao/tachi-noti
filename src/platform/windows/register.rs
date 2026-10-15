//! Per-user (HKCU, no admin) Windows identity for Tachi:
//!
//! 1. `HKCU\Software\Classes\AppUserModelId\<AUMID>` — display name + icon
//!    Windows shows on the toast and in Settings → Notifications.
//! 2. A Start menu shortcut carrying `System.AppUserModel.ID` — Microsoft's
//!    documented requirement for an unpackaged desktop app to raise toasts.
//!    The registry key alone is not documented as sufficient, so both exist.
//! 3. The `tachi-noti://` URL protocol — what a toast click launches.

use std::path::{Path, PathBuf};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{HSTRING, Interface, PCWSTR};

const SHORTCUT_DESCRIPTION: &str = "Tachi — Claude Code notifications";
const BAR_EXE: &str = "tachi-bar.exe";

fn aumid_key() -> String {
    format!(r"Software\Classes\AppUserModelId\{}", super::AUMID)
}

fn protocol_key() -> String {
    format!(r"Software\Classes\{}", crate::platform::ACTIVATION_SCHEME)
}

// ---------- pure helpers ----------

/// A path spelled for the Windows shell: backslashes kept, `\\?\` verbatim
/// prefix removed (the shell cannot launch a verbatim path).
fn native_path(path: &Path) -> String {
    let raw = path.display().to_string();
    if let Some(unc) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw
    }
}

/// Command stored under `shell\open\command`. Always quoted: install paths
/// routinely contain spaces, and `"%1"` is quoted so the URI stays one argument.
fn protocol_command(exe: &Path) -> String {
    format!("\"{}\" activate \"%1\"", native_path(exe))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Tachi.lnk`
fn shortcut_path_in(appdata: &Path) -> PathBuf {
    appdata
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join(format!("{}.lnk", super::APP_DISPLAY_NAME))
}

fn shortcut_path() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA is not set")?;
    Ok(shortcut_path_in(Path::new(&appdata)))
}

/// The GUI app (`tachi-bar.exe`, no console window) when it ships beside this
/// binary, else this binary.
fn app_exe_in(current: &Path, bar_exists: impl Fn(&Path) -> bool) -> PathBuf {
    if let Some(dir) = current.parent() {
        let bar = dir.join(BAR_EXE);
        if bar_exists(&bar) {
            return bar;
        }
    }
    current.to_path_buf()
}

fn app_exe() -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    Ok(app_exe_in(&current, |p| p.is_file()))
}

// ---------- registry ----------

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn win_err(what: &str, code: u32) -> String {
    format!("{what} failed (Win32 error {code})")
}

fn create_key(subkey: &str) -> Result<Key, String> {
    let mut hkey = HKEY::default();
    // SAFETY: out-pointer is a valid local; the subkey string outlives the call.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(subkey),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut hkey,
            None,
        )
    };
    if rc.0 != 0 {
        return Err(win_err(&format!("creating HKCU\\{subkey}"), rc.0));
    }
    Ok(Key(hkey))
}

/// Set a REG_SZ value; `name` None writes the key's (Default) value.
fn set_string(key: &Key, name: Option<&str>, value: &str) -> Result<(), String> {
    let bytes: Vec<u8> = value.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
    let name_h = name.map(HSTRING::from);
    let name_p = match &name_h {
        Some(h) => PCWSTR(h.as_ptr()),
        None => PCWSTR::null(),
    };
    // SAFETY: name_p points into name_h (alive here) or is null for (Default).
    let rc = unsafe { RegSetValueExW(key.0, name_p, None, REG_SZ, Some(&bytes)) };
    if rc.0 != 0 {
        return Err(win_err(&format!("writing registry value {}", name.unwrap_or("(Default)")), rc.0));
    }
    Ok(())
}

fn key_exists(subkey: &str) -> bool {
    let mut hkey = HKEY::default();
    // SAFETY: out-pointer is a valid local.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(subkey), None, KEY_READ, &mut hkey) };
    if rc.0 != 0 {
        return false;
    }
    drop(Key(hkey));
    true
}

fn read_string(subkey: &str, name: Option<&str>) -> Option<String> {
    let sub = HSTRING::from(subkey);
    let name_h = name.map(HSTRING::from);
    let name_p = match &name_h {
        Some(h) => PCWSTR(h.as_ptr()),
        None => PCWSTR::null(),
    };
    let mut size = 0u32;
    // SAFETY: first call only asks for the required byte size.
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &sub, name_p, RRF_RT_REG_SZ, None, None, Some(&mut size))
    };
    if rc.0 != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buf holds `size` writable bytes.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &sub,
            name_p,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if rc.0 != 0 {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    let s = String::from_utf16_lossy(&buf[..units]);
    Some(s.trim_end_matches('\0').to_string())
}

/// Delete exactly this HKCU key (and its subkeys). Ok(false) = was not there.
fn delete_tree(subkey: &str) -> Result<bool, String> {
    if !key_exists(subkey) {
        return Ok(false);
    }
    // SAFETY: plain call with an owned string.
    let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(subkey)) };
    if rc.0 != 0 {
        return Err(win_err(&format!("deleting HKCU\\{subkey}"), rc.0));
    }
    Ok(true)
}

// ---------- shortcut ----------

fn write_shortcut(lnk: &Path, target: &Path) -> Result<(), String> {
    if let Some(dir) = lnk.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    // SAFETY: standard COM init; balanced below only when it succeeded.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let result = build_shortcut(lnk, target).map_err(|e| format!("creating shortcut {}: {e}", lnk.display()));
    if init.is_ok() {
        // SAFETY: pairs with the successful CoInitializeEx above.
        unsafe { CoUninitialize() };
    }
    result
}

/// System.AppUserModel.ID is declared VT_LPWSTR. `PROPVARIANT::from(&str)`
/// builds a VT_BSTR, which the shell link's property store is not documented
/// to coerce, so build the documented type. The buffer is CoTaskMemAlloc'd
/// because PROPVARIANT's Drop (PropVariantClear) frees it with CoTaskMemFree.
fn lpwstr_propvariant(s: &str) -> windows::core::Result<PROPVARIANT> {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the allocation is exactly wide.len() u16s and fully written
    // before the variant (which takes ownership) is returned.
    unsafe {
        let buf = windows::Win32::System::Com::CoTaskMemAlloc(wide.len() * std::mem::size_of::<u16>()) as *mut u16;
        if buf.is_null() {
            return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), buf, wide.len());
        let mut pv = PROPVARIANT::default();
        let inner = &mut *pv.Anonymous.Anonymous;
        inner.vt = windows::Win32::System::Variant::VT_LPWSTR;
        inner.Anonymous.pwszVal = windows::core::PWSTR(buf);
        Ok(pv)
    }
}

fn build_shortcut(lnk: &Path, target: &Path) -> windows::core::Result<()> {
    // SAFETY: COM calls on interfaces we own; every string outlives its call.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(target.as_os_str()))?;
        link.SetArguments(&HSTRING::new())?;
        link.SetDescription(&HSTRING::from(SHORTCUT_DESCRIPTION))?;

        let store: IPropertyStore = link.cast()?;
        let id = lpwstr_propvariant(super::AUMID)?;
        store.SetValue(&PKEY_AppUserModel_ID, &id)?;
        store.Commit()?;

        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)
    }
}

// ---------- contract ----------

/// Identity registered: the AUMID key exists and the shortcut is on disk.
pub fn is_registered() -> bool {
    key_exists(&aumid_key()) && shortcut_path().map(|p| p.is_file()).unwrap_or(false)
}

pub fn install() -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let exe = app_exe()?;

    let key = create_key(&aumid_key())?;
    set_string(&key, Some("DisplayName"), super::APP_DISPLAY_NAME)?;
    if let Some(icon) = crate::notify::default_icon() {
        set_string(&key, Some("IconUri"), &native_path(&icon))?;
    }
    lines.push(format!("registered AUMID {} (HKCU\\{})", super::AUMID, aumid_key()));

    let lnk = shortcut_path()?;
    write_shortcut(&lnk, &exe)?;
    lines.push(format!("Start menu shortcut {} -> {}", lnk.display(), native_path(&exe)));

    let proto = create_key(&protocol_key())?;
    set_string(&proto, None, "URL:Tachi Noti")?;
    set_string(&proto, Some("URL Protocol"), "")?;
    let cmd_key = create_key(&format!(r"{}\shell\open\command", protocol_key()))?;
    let command = protocol_command(&exe);
    set_string(&cmd_key, None, &command)?;
    lines.push(format!("URL protocol {}:// -> {command}", crate::platform::ACTIVATION_SCHEME));

    Ok(lines)
}

pub fn uninstall() -> Result<Vec<String>, String> {
    let mut lines = Vec::new();

    let lnk = shortcut_path()?;
    if lnk.is_file() {
        std::fs::remove_file(&lnk).map_err(|e| format!("removing {}: {e}", lnk.display()))?;
        lines.push(format!("removed Start menu shortcut {}", lnk.display()));
    }
    for key in [aumid_key(), protocol_key()] {
        if delete_tree(&key)? {
            lines.push(format!("removed HKCU\\{key}"));
        }
    }
    Ok(lines)
}

pub fn status() -> Vec<String> {
    let mut lines = Vec::new();

    if key_exists(&aumid_key()) {
        lines.push(format!("AUMID {} registered", super::AUMID));
    } else {
        lines.push(format!("AUMID {} not registered", super::AUMID));
    }

    match shortcut_path() {
        Ok(p) => {
            let state = if p.is_file() { "present" } else { "missing" };
            lines.push(format!("Start menu shortcut: {} ({state})", p.display()));
        }
        Err(e) => lines.push(format!("Start menu shortcut: unknown ({e})")),
    }

    let cmd_key = format!(r"{}\shell\open\command", protocol_key());
    match read_string(&cmd_key, None) {
        Some(c) => lines.push(format!("URL protocol {} → {c}", crate::platform::ACTIVATION_SCHEME)),
        None => lines.push(format!("URL protocol {} → missing", crate::platform::ACTIVATION_SCHEME)),
    }
    lines
}

/// Target of a toast click: `tachi-noti activate <uri>`. Never panics; a URI
/// we cannot parse is logged and dropped.
pub fn handle_activation(uri: &str) {
    match crate::platform::parse_activation_uri(uri) {
        Some(a) => super::host::focus_host(&a.host_id, a.open_path.as_deref(), a.window.as_deref()),
        None => crate::hook::debug_log(&format!("activate: rejected uri {uri:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_command_quotes_exe_with_spaces() {
        let cmd = protocol_command(Path::new(r"C:\Program Files\Tachi Noti\tachi-bar.exe"));
        assert_eq!(cmd, r#""C:\Program Files\Tachi Noti\tachi-bar.exe" activate "%1""#);
    }

    #[test]
    fn native_path_strips_verbatim_prefix_keeps_backslashes() {
        assert_eq!(native_path(Path::new(r"\\?\C:\a b\x.exe")), r"C:\a b\x.exe");
        assert_eq!(native_path(Path::new(r"\\?\UNC\srv\share\x.exe")), r"\\srv\share\x.exe");
        assert_eq!(native_path(Path::new(r"C:\a\x.exe")), r"C:\a\x.exe");
    }

    #[test]
    fn shortcut_lives_in_start_menu_programs() {
        let p = shortcut_path_in(Path::new(r"C:\Users\me\AppData\Roaming"));
        assert_eq!(
            p,
            Path::new(r"C:\Users\me\AppData\Roaming").join("Microsoft").join("Windows").join("Start Menu").join("Programs").join("Tachi.lnk")
        );
    }

    #[test]
    fn app_exe_prefers_sibling_bar() {
        let current = Path::new(r"C:\t\tachi-noti.exe");
        assert_eq!(app_exe_in(current, |_| true), Path::new(r"C:\t").join("tachi-bar.exe"));
        assert_eq!(app_exe_in(current, |_| false), current);
    }

    #[test]
    fn registry_keys_are_the_documented_ones() {
        assert_eq!(aumid_key(), r"Software\Classes\AppUserModelId\TachiNoti.Tachi");
        assert_eq!(protocol_key(), r"Software\Classes\tachi-noti");
    }
}
