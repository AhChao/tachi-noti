//! Host app (terminal / IDE) identification and focus on Windows. The host id
//! is the full path of the GUI process that owns the session's window; it is
//! persisted as `bundle_id` and handed back to `focus_host`.

use std::collections::HashSet;
use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::System::Console::GetConsoleWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VK_F24,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, FlashWindowEx, GetAncestor, GetForegroundWindow,
    GetWindow, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    SetForegroundWindow, ShowWindow, ASFW_ANY, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY, GA_ROOT,
    GW_OWNER, SW_RESTORE,
};

use super::proc;
use crate::platform::path_basename;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_HOPS: usize = 20;

/// Console plumbing that may own windows but is never the app the user sits in.
const CONSOLE_PLUMBING: &[&str] = &["conhost.exe", "openconsole.exe"];
const DESKTOP_SHELL: &str = "explorer.exe";

/// VS Code family: single-instance apps that take a folder argument and focus
/// the window already showing it.
const WINDOW_PER_FOLDER_EXES: &[&str] = &[
    "code.exe",
    "code - insiders.exe",
    "vscodium.exe",
    "cursor.exe",
    "windsurf.exe",
    "antigravity.exe",
];

/// A visible, unowned top-level window.
#[derive(Clone, Copy)]
struct TopWindow {
    hwnd: isize,
    pid: u32,
}

fn to_hwnd(h: isize) -> HWND {
    HWND(h as *mut c_void)
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let out = unsafe { &mut *(lparam.0 as *mut Vec<TopWindow>) };
    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let owned = unsafe { GetWindow(hwnd, GW_OWNER) }.map(|o| !o.is_invalid()).unwrap_or(false);
        if !owned {
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
            if pid != 0 {
                out.push(TopWindow { hwnd: hwnd.0 as isize, pid });
            }
        }
    }
    BOOL::from(true)
}

/// Top-level windows in z-order, topmost first.
fn top_windows() -> Vec<TopWindow> {
    let mut out: Vec<TopWindow> = Vec::new();
    let _ = unsafe { EnumWindows(Some(collect_window), LPARAM(&mut out as *mut _ as isize)) };
    out
}

fn window_pid(hwnd: HWND) -> Option<u32> {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

fn path_string(p: PathBuf) -> String {
    p.to_string_lossy().into_owned()
}

fn base_lower(path: &str) -> String {
    path_basename(path).to_lowercase()
}

/// Outcome of walking the ancestor chain looking for the window owner.
#[derive(Debug, PartialEq, Eq)]
enum HostPick {
    Found(String),
    /// Reached Explorer or the chain ended without a window-owning process.
    NotFound,
}

/// Pure core of `session_host_id`: the nearest ancestor that owns a window,
/// skipping console plumbing, and giving up at Explorer (the desktop) so we
/// never claim Explorer hosts the session.
fn pick_host(
    chain: &[u32],
    window_pids: &HashSet<u32>,
    exe: impl Fn(u32) -> Option<String>,
) -> HostPick {
    for &pid in chain {
        let Some(path) = exe(pid) else { continue };
        let name = base_lower(&path);
        if name == DESKTOP_SHELL {
            return HostPick::NotFound;
        }
        if CONSOLE_PLUMBING.contains(&name.as_str()) {
            continue;
        }
        if window_pids.contains(&pid) {
            return HostPick::Found(path);
        }
    }
    HostPick::NotFound
}

/// Full path of the GUI process hosting this session. For a standalone console
/// (cmd / PowerShell started on its own) the window belongs to conhost, not to
/// a shell ancestor, so the walk finds nothing; then the console's own window
/// is used when it is visible. Under ConPTY (Windows Terminal, VS Code) that
/// window is a hidden pseudo-window, so it is ignored and the walk result stands.
pub fn session_host_id() -> Option<String> {
    let window_pids: HashSet<u32> = top_windows().into_iter().map(|w| w.pid).collect();
    let chain = proc::ancestors(std::process::id(), MAX_HOPS);
    let picked = pick_host(&chain, &window_pids, |pid| proc::exe_path(pid).map(path_string));
    if let HostPick::Found(path) = picked {
        return Some(path);
    }
    let console = unsafe { GetConsoleWindow() };
    if console.is_invalid() || !unsafe { IsWindowVisible(console) }.as_bool() {
        return None;
    }
    proc::exe_path(window_pid(console)?).map(path_string)
}

pub fn frontmost_host_id() -> Option<String> {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    proc::exe_path(window_pid(fg)?).map(path_string)
}

/// Case-insensitive path equality ignoring separator style and a `\\?\` prefix.
fn normalize(p: &str) -> String {
    let p = p.replace('/', "\\");
    let p = p.strip_prefix(r"\\?\").unwrap_or(&p);
    p.to_lowercase()
}

pub fn same_host(a: &str, b: &str) -> bool {
    normalize(a) == normalize(b)
}

pub fn window_per_folder(host_id: &str) -> bool {
    WINDOW_PER_FOLDER_EXES.contains(&base_lower(host_id).as_str())
}

pub fn window_per_folder_hook_hint() -> bool {
    std::env::var("TERM_PROGRAM").map(|t| t == "vscode").unwrap_or(false)
}

/// First (topmost) window whose owning executable is the host. Input is in
/// z-order, so "first" is the most recently active one.
fn select_host_window(windows: &[(isize, Option<String>)], host_id: &str) -> Option<isize> {
    windows
        .iter()
        .find(|(_, exe)| exe.as_deref().is_some_and(|e| same_host(e, host_id)))
        .map(|(h, _)| *h)
}

/// Window token: the decimal HWND of a top-level window; zero or garbage is rejected.
fn parse_window_token(token: &str) -> Option<isize> {
    token.trim().parse::<isize>().ok().filter(|h| *h != 0)
}

fn root_of(hwnd: HWND) -> HWND {
    unsafe { GetAncestor(hwnd, GA_ROOT) }
}

/// Token of the window the user is in right now, only if it belongs to
/// `host_id`; the user may have switched away before the hook ran, and a wrong
/// window is worse than none.
pub fn current_window_token(host_id: Option<&str>) -> Option<String> {
    let host_id = host_id?;
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    let root = root_of(fg);
    if root.is_invalid() {
        return None;
    }
    let exe = path_string(proc::exe_path(window_pid(root)?)?);
    same_host(&exe, host_id).then(|| (root.0 as isize).to_string())
}

/// `None` when the window is gone, so the caller falls back to app-level.
pub fn window_frontmost(token: &str) -> Option<bool> {
    let hwnd = to_hwnd(parse_window_token(token)?);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    let fg = unsafe { GetForegroundWindow() };
    Some(!fg.is_invalid() && root_of(fg) == hwnd)
}

/// The token's window, if it still exists and still belongs to the host
/// (HWND values get reused by other apps).
fn live_token_window(token: &str, host_id: &str) -> Option<HWND> {
    let hwnd = to_hwnd(parse_window_token(token)?);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return None;
    }
    let exe = path_string(proc::exe_path(window_pid(hwnd)?)?);
    same_host(&exe, host_id).then_some(hwnd)
}

/// Bring the session's window forward. A valid window token wins (exact
/// window); otherwise VS Code-family hosts get the folder so the running
/// instance focuses the window that already has it open, and anything else gets
/// its most recent top-level window raised. Errors are ignored.
pub fn focus_host(host_id: &str, open_path: Option<&str>, window: Option<&str>) {
    if let Some(hwnd) = window.and_then(|t| live_token_window(t, host_id)) {
        raise_window(hwnd);
        return;
    }
    if let Some(path) = open_path {
        if window_per_folder(host_id) && launch_with_folder(host_id, path) {
            return;
        }
    }
    focus_window_of(host_id);
}

fn launch_with_folder(exe: &str, folder: &str) -> bool {
    // Lets the IDE process we are about to start take the foreground.
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    Command::new(exe).arg(folder).creation_flags(CREATE_NO_WINDOW).spawn().is_ok()
}

fn focus_window_of(host_id: &str) {
    let mut exe_by_pid: std::collections::HashMap<u32, Option<String>> = Default::default();
    let windows: Vec<(isize, Option<String>)> = top_windows()
        .into_iter()
        .map(|w| {
            let exe = exe_by_pid
                .entry(w.pid)
                .or_insert_with(|| proc::exe_path(w.pid).map(path_string))
                .clone();
            (w.hwnd, exe)
        })
        .collect();
    if let Some(target) = select_host_window(&windows, host_id).map(to_hwnd) {
        raise_window(target);
    }
}

fn raise_window(target: HWND) {
    unsafe {
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }
        if SetForegroundWindow(target).as_bool() {
            return;
        }
        // The foreground lock refused. A synthetic key event makes this
        // process the last one to receive input, which re-qualifies it.
        nudge_input();
        if SetForegroundWindow(target).as_bool() {
            return;
        }
        let info = FLASHWINFO {
            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
            hwnd: target,
            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
            uCount: 0,
            dwTimeout: 0,
        };
        let _ = FlashWindowEx(&info);
    }
}

/// F24 press/release: counts as input but no application binds it, unlike
/// ALT, whose release would focus the target's menu bar.
fn nudge_input() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VK_F24, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

#[cfg(test)]
mod tests {
    use super::*;

    const CODE: &str = r"C:\Users\me\AppData\Local\Programs\Microsoft VS Code\Code.exe";
    const WT: &str = r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal_1\WindowsTerminal.exe";

    #[test]
    fn same_host_ignores_case_separators_and_prefix() {
        assert!(same_host(CODE, &CODE.to_uppercase()));
        assert!(same_host(CODE, &CODE.replace('\\', "/")));
        assert!(same_host(&format!(r"\\?\{CODE}"), CODE));
        assert!(!same_host(CODE, WT));
    }

    #[test]
    fn window_per_folder_recognises_vscode_family() {
        for name in ["Code.exe", "Code - Insiders.exe", "VSCodium.exe", "Cursor.exe", "Windsurf.exe", "Antigravity.exe"] {
            assert!(window_per_folder(&format!(r"C:\Apps\{name}")), "{name}");
        }
        assert!(window_per_folder(r"c:\apps\CODE.EXE"));
        assert!(!window_per_folder(WT));
        assert!(!window_per_folder(r"C:\Apps\Code.exe.bak"));
    }

    fn exe_table(pid: u32) -> Option<String> {
        Some(match pid {
            1 => r"C:\Windows\System32\cmd.exe".into(),
            2 => r"C:\Windows\System32\conhost.exe".into(),
            3 => WT.into(),
            4 => CODE.into(),
            5 => r"C:\Windows\explorer.exe".into(),
            _ => return None,
        })
    }

    #[test]
    fn pick_host_returns_nearest_window_owner() {
        let wins: HashSet<u32> = [3, 4, 5].into_iter().collect();
        assert_eq!(pick_host(&[1, 3, 4, 5], &wins, exe_table), HostPick::Found(WT.into()));
    }

    #[test]
    fn pick_host_skips_console_plumbing_even_with_window() {
        let wins: HashSet<u32> = [2, 4].into_iter().collect();
        assert_eq!(pick_host(&[1, 2, 4], &wins, exe_table), HostPick::Found(CODE.into()));
    }

    #[test]
    fn pick_host_stops_at_explorer() {
        let wins: HashSet<u32> = [5, 4].into_iter().collect();
        assert_eq!(pick_host(&[1, 5, 4], &wins, exe_table), HostPick::NotFound);
    }

    #[test]
    fn pick_host_tolerates_unreadable_ancestors() {
        let wins: HashSet<u32> = [4].into_iter().collect();
        assert_eq!(pick_host(&[99, 4], &wins, exe_table), HostPick::Found(CODE.into()));
        assert_eq!(pick_host(&[], &wins, exe_table), HostPick::NotFound);
    }

    #[test]
    fn parse_window_token_accepts_only_nonzero_integers() {
        assert_eq!(parse_window_token("65872"), Some(65872));
        assert_eq!(parse_window_token(" 65872\n"), Some(65872));
        for bad in ["", "0", "abc", "12x", "-", "0x10", "1.5"] {
            assert_eq!(parse_window_token(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn select_host_window_takes_topmost_match() {
        let windows = vec![
            (10, Some(WT.to_string())),
            (11, None),
            (12, Some(CODE.to_string())),
            (13, Some(CODE.to_lowercase())),
        ];
        assert_eq!(select_host_window(&windows, CODE), Some(12));
        assert_eq!(select_host_window(&windows, WT), Some(10));
        assert_eq!(select_host_window(&windows, r"C:\other.exe"), None);
    }
}
