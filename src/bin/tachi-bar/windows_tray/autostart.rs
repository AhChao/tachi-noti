//! Launch at login via HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//! (per user, no admin). The value is the quoted path of the running exe.

use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "TachiBar";

/// `"C:\path\tachi-bar.exe"`; drops the `\\?\` verbatim prefix that
/// `current_exe` / canonicalize can add (Run entries do not understand it).
pub fn command_for(exe: &str) -> String {
    let plain = match exe.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => exe.strip_prefix(r"\\?\").unwrap_or(exe).to_string(),
    };
    format!("\"{plain}\"")
}

/// Case-insensitive comparison of two Run commands, ignoring surrounding quotes.
fn same_command(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.trim().trim_matches('"').to_lowercase();
    norm(a) == norm(b)
}

fn current_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate this executable: {e}"))?;
    Ok(command_for(&exe.to_string_lossy()))
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by us and is closed exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn read_registered() -> Option<String> {
    let sub = HSTRING::from(RUN_KEY);
    let name = HSTRING::from(VALUE_NAME);
    let mut size = 0u32;
    // SAFETY: the first call only asks for the required byte size.
    let rc = unsafe { RegGetValueW(HKEY_CURRENT_USER, &sub, &name, RRF_RT_REG_SZ, None, None, Some(&mut size)) };
    if rc.0 != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u16; (size as usize).div_ceil(2)];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: buf holds `size` writable bytes.
    let rc = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, &sub, &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut size))
    };
    if rc.0 != 0 {
        return None;
    }
    let units = (size as usize / 2).min(buf.len());
    Some(String::from_utf16_lossy(&buf[..units]).trim_end_matches('\0').to_string())
}

/// True when the Run entry exists and launches this very executable.
pub fn is_enabled() -> bool {
    match (read_registered(), current_command()) {
        (Some(registered), Ok(ours)) => same_command(&registered, &ours),
        _ => false,
    }
}

/// Write the Run entry; returns the command that was registered.
pub fn enable() -> Result<String, String> {
    let cmd = current_command()?;
    let mut hkey = HKEY::default();
    // SAFETY: out-pointer is a valid local; the subkey string outlives the call.
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(RUN_KEY),
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
        return Err(format!("opening HKCU\\{RUN_KEY} failed (Win32 error {})", rc.0));
    }
    let key = Key(hkey);
    let bytes: Vec<u8> = cmd.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
    // SAFETY: the name string outlives the call; bytes is a NUL-terminated UTF-16 buffer.
    let rc = unsafe { RegSetValueExW(key.0, &HSTRING::from(VALUE_NAME), None, REG_SZ, Some(&bytes)) };
    if rc.0 != 0 {
        return Err(format!("writing the {VALUE_NAME} Run entry failed (Win32 error {})", rc.0));
    }
    Ok(cmd)
}

/// Remove the Run entry. Ok(false) = there was none.
pub fn disable() -> Result<bool, String> {
    let mut hkey = HKEY::default();
    // SAFETY: out-pointer is a valid local.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(RUN_KEY), None, KEY_SET_VALUE, &mut hkey) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if rc.0 != 0 {
        return Err(format!("opening HKCU\\{RUN_KEY} failed (Win32 error {})", rc.0));
    }
    let key = Key(hkey);
    // SAFETY: plain call with an owned string.
    let rc = unsafe { RegDeleteValueW(key.0, &HSTRING::from(VALUE_NAME)) };
    if rc == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if rc.0 != 0 {
        return Err(format!("deleting the {VALUE_NAME} Run entry failed (Win32 error {})", rc.0));
    }
    Ok(true)
}

/// CLI: `tachi-bar install-agent`.
pub fn install() {
    match enable() {
        Ok(cmd) => println!("Enabled launch at login (HKCU Run \u{2192} {cmd})"),
        Err(e) => eprintln!("tachi-bar: {e}"),
    }
}

/// CLI: `tachi-bar uninstall-agent`.
pub fn uninstall() {
    match disable() {
        Ok(true) => println!("Disabled launch at login"),
        Ok(false) => println!("Launch at login was not enabled \u{2014} nothing to do."),
        Err(e) => eprintln!("tachi-bar: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_a_plain_path() {
        assert_eq!(command_for(r"C:\Users\me\tachi-bar.exe"), r#""C:\Users\me\tachi-bar.exe""#);
    }

    #[test]
    fn strips_the_verbatim_prefix() {
        assert_eq!(command_for(r"\\?\C:\Program Files\Tachi\tachi-bar.exe"), r#""C:\Program Files\Tachi\tachi-bar.exe""#);
        assert_eq!(command_for(r"\\?\UNC\srv\share\tachi-bar.exe"), r#""\\srv\share\tachi-bar.exe""#);
    }

    #[test]
    fn comparison_ignores_case_and_quotes() {
        assert!(same_command(r#""C:\Tachi\TACHI-BAR.exe""#, r"c:\tachi\tachi-bar.exe"));
        assert!(!same_command(r#""C:\Tachi\tachi-bar.exe""#, r#""C:\Other\tachi-bar.exe""#));
    }
}
