//! Stdio MCP process lifecycle helpers (plans A/B/C).
//!
//! - **A**: hard-exit watchdog so Drop/cleanup cannot hang the process forever
//! - **B**: optional parent-process watch (disable with `OBSCURA_MCP_NO_PARENT_WATCH=1`)
//! - **C**: recognize explicit shutdown methods/notifications

use std::time::Duration;

/// If graceful cleanup stalls (e.g. native Drop), force process exit after this.
pub const HARD_EXIT_AFTER: Duration = Duration::from_millis(1000);

/// Parent liveness poll interval.
const PARENT_POLL: Duration = Duration::from_secs(1);

/// Arm a daemon thread that calls `process::exit(0)` if still running after `after`.
/// Uses a plain OS thread so it still fires if the Tokio runtime is wedged.
pub fn arm_hard_exit(after: Duration) {
    std::thread::Builder::new()
        .name("obscura-mcp-hard-exit".into())
        .spawn(move || {
            std::thread::sleep(after);
            eprintln!(
                "obscura-mcp: hard-exit after {}ms shutdown timeout",
                after.as_millis()
            );
            std::process::exit(0);
        })
        .ok();
}

/// Methods (request or notification) that mean "host is done; exit the server".
pub fn is_shutdown_method(method: &str) -> bool {
    matches!(
        method,
        "exit"
            | "shutdown"
            | "notifications/exit"
            | "notifications/shutdown"
            // Some clients use a bare custom notification name:
            | "exit/notification"
    )
}

pub fn parent_watch_enabled() -> bool {
    match std::env::var("OBSCURA_MCP_NO_PARENT_WATCH") {
        Ok(v) => {
            let v = v.trim();
            !(v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes"))
        }
        Err(_) => true,
    }
}

/// Spawn a task that completes when the parent process is no longer alive.
/// Returns `None` if parent cannot be determined or watch is disabled.
pub fn spawn_parent_death_watch() -> Option<tokio::task::JoinHandle<()>> {
    if !parent_watch_enabled() {
        return None;
    }
    let ppid = parent_pid()?;
    // Never treat pid 0/self as parent.
    let self_pid = std::process::id();
    if ppid == 0 || ppid == self_pid {
        return None;
    }
    Some(tokio::spawn(async move {
        loop {
            if !process_alive(ppid) {
                return;
            }
            tokio::time::sleep(PARENT_POLL).await;
        }
    }))
}

#[cfg(windows)]
fn parent_pid() -> Option<u32> {
    windows_parent_pid()
}

#[cfg(unix)]
fn parent_pid() -> Option<u32> {
    // SAFETY: getppid is always safe.
    Some(unsafe { libc_getppid() })
}

#[cfg(not(any(windows, unix)))]
fn parent_pid() -> Option<u32> {
    None
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    windows_process_alive(pid)
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0): exists (or EPERM) => alive; ESRCH => dead.
    // Avoid libc dep: use `kill -0` via /bin/kill when available.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true) // if we cannot check, do not force exit
}

#[cfg(not(any(windows, unix)))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(unix)]
fn libc_getppid() -> u32 {
    // Minimal extern to avoid adding the `libc` crate to the workspace.
    extern "C" {
        fn getppid() -> i32;
    }
    // SAFETY: getppid has no preconditions.
    unsafe { getppid() as u32 }
}

#[cfg(windows)]
fn windows_parent_pid() -> Option<u32> {
    use std::mem::{size_of, zeroed};

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut std::ffi::c_void;
        fn Process32FirstW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: *mut std::ffi::c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        fn GetCurrentProcessId() -> u32;
    }

    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const INVALID_HANDLE_VALUE: isize = -1;

    unsafe {
        let self_pid = GetCurrentProcessId();
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap.is_null() || snap as isize == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: ProcessEntry32W = zeroed();
        entry.dw_size = size_of::<ProcessEntry32W>() as u32;
        let mut found = None;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == self_pid {
                    found = Some(entry.th32_parent_process_id);
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(windows)]
fn windows_process_alive(pid: u32) -> bool {
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
        fn GetExitCodeProcess(handle: *mut std::ffi::c_void, code: *mut u32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_methods_recognized() {
        assert!(is_shutdown_method("exit"));
        assert!(is_shutdown_method("shutdown"));
        assert!(is_shutdown_method("notifications/exit"));
        assert!(is_shutdown_method("notifications/shutdown"));
        assert!(!is_shutdown_method("notifications/initialized"));
        assert!(!is_shutdown_method("ping"));
    }
}
