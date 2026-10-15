//! Process introspection on Windows via Toolhelp and the limited-query
//! process rights (no admin, works for same-user processes).

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `STILL_ACTIVE` exit code reported while a process runs.
const STILL_ACTIVE_CODE: u32 = 259;

/// Closes the wrapped handle on drop, so every early return releases it.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open_limited(pid: u32) -> Option<OwnedHandle> {
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(OwnedHandle)
}

/// One Toolhelp snapshot parsed into a pid -> parent pid map.
fn parent_map() -> HashMap<u32, u32> {
    let mut map = HashMap::new();
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return map;
    };
    let snap = OwnedHandle(snap);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = unsafe { Process32FirstW(snap.0, &mut entry) }.is_ok();
    while more {
        map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        more = unsafe { Process32NextW(snap.0, &mut entry) }.is_ok();
    }
    map
}

fn is_system_pid(pid: u32) -> bool {
    pid == 0 || pid == 4
}

pub fn parent_pid(pid: u32) -> Option<u32> {
    if is_system_pid(pid) {
        return None;
    }
    parent_map().get(&pid).copied().filter(|p| *p != 0)
}

pub fn self_parent_pid() -> u32 {
    parent_pid(std::process::id()).unwrap_or(0)
}

/// Full Win32 image path of the process, if it can be queried.
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    let h = open_limited(pid)?;
    // 32767 UTF-16 units is the NT path limit, so one call never truncates.
    let mut buf = vec![0u16; 32768];
    let mut len = buf.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    }
    .ok()?;
    if len == 0 {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..len as usize])))
}

/// A process we cannot open (other user, protected) counts as dead, the same
/// "not ours" rule the macOS version applies.
pub fn pid_alive(pid: u32) -> bool {
    if is_system_pid(pid) {
        return false;
    }
    let Some(h) = open_limited(pid) else { return false };
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(h.0, &mut code) }.is_ok() && code == STILL_ACTIVE_CODE
}

/// Process creation time as a 64-bit FILETIME tick count.
fn creation_time(pid: u32) -> Option<u64> {
    let h = open_limited(pid)?;
    let (mut created, mut a, mut b, mut c) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    unsafe { GetProcessTimes(h.0, &mut created, &mut a, &mut b, &mut c) }.ok()?;
    Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
}

/// Parents of `pid`, nearest first, excluding `pid` itself, at most `max`
/// entries. Built on a single snapshot rather than one per hop. Windows reuses
/// pids and never rewrites a child's recorded parent, so a stale parent pid can
/// name an unrelated newer process; the walk stops at a revisit or when the
/// "parent" was created after its child.
pub fn ancestors(pid: u32, max: usize) -> Vec<u32> {
    let map = parent_map();
    walk_ancestors(&map, pid, max, creation_time)
}

/// Pure walker behind `ancestors`: `created` supplies creation times (None =
/// unknown, which does not stop the walk).
fn walk_ancestors(
    parents: &HashMap<u32, u32>,
    start: u32,
    max: usize,
    created: impl Fn(u32) -> Option<u64>,
) -> Vec<u32> {
    let mut out = Vec::new();
    let mut seen = vec![start];
    let mut cur = start;
    while out.len() < max {
        let Some(&parent) = parents.get(&cur) else { break };
        if is_system_pid(parent) || seen.contains(&parent) {
            break;
        }
        if let (Some(child_t), Some(parent_t)) = (created(cur), created(parent)) {
            if parent_t > child_t {
                break;
            }
        }
        seen.push(parent);
        out.push(parent);
        cur = parent;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(u32, u32)]) -> HashMap<u32, u32> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn walks_chain_nearest_first_and_stops_at_system() {
        let m = map(&[(10, 20), (20, 30), (30, 4), (4, 0)]);
        assert_eq!(walk_ancestors(&m, 10, 20, |_| None), vec![20, 30]);
    }

    #[test]
    fn respects_max_hops() {
        let m = map(&[(1, 2), (2, 3), (3, 104), (104, 105)]);
        assert_eq!(walk_ancestors(&m, 1, 2, |_| None), vec![2, 3]);
    }

    #[test]
    fn stops_on_cycle() {
        let m = map(&[(10, 20), (20, 30), (30, 10)]);
        assert_eq!(walk_ancestors(&m, 10, 20, |_| None), vec![20, 30]);
    }

    #[test]
    fn stops_when_parent_is_newer_than_child() {
        // 30 is a recycled pid created after 20, so it cannot be 20's parent.
        let m = map(&[(10, 20), (20, 30), (30, 40)]);
        let times = |p: u32| match p {
            10 => Some(300),
            20 => Some(200),
            30 => Some(900),
            _ => Some(100),
        };
        assert_eq!(walk_ancestors(&m, 10, 20, times), vec![20]);
    }

    #[test]
    fn unknown_pid_has_no_ancestors() {
        assert!(walk_ancestors(&HashMap::new(), 10, 20, |_| None).is_empty());
    }
}
