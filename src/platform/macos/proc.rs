use std::path::PathBuf;

/// Parent pid of a process via libproc — no subprocess spawned.
pub fn parent_pid(pid: u32) -> Option<u32> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if rc != size {
        return None;
    }
    Some(info.pbi_ppid)
}

pub fn self_parent_pid() -> u32 {
    std::os::unix::process::parent_id()
}

/// Executable path of a process (resolved, not argv[0]).
pub fn exe_path(pid: u32) -> Option<PathBuf> {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let rc = unsafe {
        libc::proc_pidpath(pid as libc::c_int, buf.as_mut_ptr() as *mut libc::c_void, buf.len() as u32)
    };
    if rc <= 0 {
        return None;
    }
    use std::os::unix::ffi::OsStrExt;
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..rc as usize])))
}

/// True when the pid exists and is ours to signal. EPERM (exists, other
/// user) counts as dead: our same-user Claude can't have become that.
pub fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_introspection_resolves_self_and_rejects_bogus_pid() {
        assert!(parent_pid(std::process::id()).expect("own process must resolve") > 0);
        let path = exe_path(std::process::id()).expect("own path must resolve");
        assert!(path.is_absolute());
        // macOS pid_max is 99999 — far beyond it can never exist.
        assert!(parent_pid(4_000_000).is_none());
        assert!(exe_path(4_000_000).is_none());
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(4_000_000));
    }
}
