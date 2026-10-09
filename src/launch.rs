//! Starting what a rule runs.
//!
//! `run` is a process started directly, with no shell.  `open` is handed
//! to the OS: `open` on macOS, `ShellExecuteExW` on Windows, `xdg-open`
//! elsewhere.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::config::Command;

/// Where a started program's output goes.
pub enum Output {
    /// This process's own: for `shiodoki try`, run from a terminal.
    Inherit,
    /// Appended to a log file: for the agent.
    Log(PathBuf),
    /// Thrown away: for the checks the agent runs again and again.
    Discard,
}

pub enum Started {
    /// A process to wait for, for its exit status.
    Child(Child),
    /// Handed to the OS, with nothing to wait for.
    HandedOff,
}

/// How a process ended, in the same words on every OS: `exit status: 4`,
/// where the standard library says `exit code: 4` on Windows.  The log is
/// read back by `shiodoki try --agent`, so the words are part of how it
/// knows a run went well.  A Unix process ended by a signal keeps the
/// standard library's words for it.
pub fn ended(status: ExitStatus) -> String {
    match status.code() {
        // A Windows exit code with the high bit set is an exception, which
        // is known by its hex.
        Some(code) if cfg!(windows) && code < 0 => format!("exit status: {:#x}", code as u32),
        Some(code) => format!("exit status: {code}"),
        None => status.to_string(),
    }
}

pub fn start(
    cmd: &Command,
    env: &BTreeMap<String, String>,
    out: &Output,
) -> Result<Started, String> {
    match cmd {
        Command::Run { argv, window } => {
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..]);
            no_console(&mut c, *window);
            spawn(c, env, out, &argv[0])
        }
        Command::Open { target, with } => open(target, with.as_deref(), env, out),
    }
}

/// How long the command of an `if` or an `unless` may take before it counts
/// as no answer.
pub const CHECK_LIMIT: Duration = Duration::from_secs(10);

/// Run the command of an `if` or an `unless`, with no window, and wait for
/// it -- for at most `limit`: how it ended, or why there is no answer.
pub fn check(
    argv: &[String],
    env: &BTreeMap<String, String>,
    out: &Output,
    limit: Duration,
) -> Result<ExitStatus, String> {
    let mut c = std::process::Command::new(&argv[0]);
    c.args(&argv[1..]);
    no_console(&mut c, false);
    let mut child = spawn_child(c, env, out, &argv[0])?;
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("{}: {e}", argv[0]))? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("no answer within {limit:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn(
    c: std::process::Command,
    env: &BTreeMap<String, String>,
    out: &Output,
    name: &str,
) -> Result<Started, String> {
    spawn_child(c, env, out, name).map(Started::Child)
}

fn spawn_child(
    mut c: std::process::Command,
    env: &BTreeMap<String, String>,
    out: &Output,
    name: &str,
) -> Result<Child, String> {
    c.envs(env).stdin(Stdio::null());
    // On Windows, HOME is whatever a POSIX shell such as MSYS2's set it to
    // -- `/home/you`, which Windows cannot start a process in.
    let home = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    if let Some(dir) = std::env::var_os(home).filter(|d| std::path::Path::new(d).is_dir()) {
        c.current_dir(dir);
    }
    match out {
        Output::Inherit => {}
        Output::Log(path) => {
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let log2 = log.try_clone().map_err(|e| e.to_string())?;
            c.stdout(log).stderr(log2);
        }
        Output::Discard => {
            c.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }
    c.spawn().map_err(|e| format!("{name}: {e}"))
}

#[cfg(windows)]
fn no_console(c: &mut std::process::Command, window: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(if window {
        CREATE_NEW_CONSOLE
    } else {
        CREATE_NO_WINDOW
    });
}

#[cfg(not(windows))]
fn no_console(_: &mut std::process::Command, _: bool) {}

#[cfg(target_os = "macos")]
fn open(
    target: &str,
    with: Option<&str>,
    env: &BTreeMap<String, String>,
    out: &Output,
) -> Result<Started, String> {
    let mut c = std::process::Command::new("/usr/bin/open");
    if let Some(app) = with {
        c.arg("-a").arg(app);
    }
    c.arg(target);
    spawn(c, env, out, "open")
}

#[cfg(windows)]
fn open(
    target: &str,
    with: Option<&str>,
    _: &BTreeMap<String, String>,
    _: &Output,
) -> Result<Started, String> {
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    let (file, params) = match with {
        Some(app) => (
            wide(app),
            Some(wide(&format!("\"{}\"", target.replace('"', "\\\"")))),
        ),
        None => (wide(target), None),
    };
    let verb = wide("open");
    let before = front::windows();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
        nShow: SW_SHOWNORMAL,
        ..Default::default()
    };
    // SAFETY: every pointer in `info` is to a NUL-terminated buffer that
    // outlives the call, or null where the API allows it.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(format!(
            "could not open {target:?}{} (ShellExecute: {})",
            with.map_or(String::new(), |w| format!(" with {w}")),
            std::io::Error::last_os_error()
        ));
    }
    // No process when the target went to a program already running.
    if !info.hProcess.is_null() {
        front::follow(info.hProcess, before);
    }
    Ok(Started::HandedOff)
}

/// Bringing what was opened to the front.
///
/// Windows lets only the program in front put a window in front, and the
/// agent never is, so what it starts opens behind the windows there are.
/// Some programs do not finish starting until their window is activated: a
/// browser built on Firefox has been seen to show a blank window, drop the
/// page it was given, and save that empty window over the tabs it had.  So
/// the agent brings the new window forward itself, as `open` on macOS
/// brings the application.
#[cfg(windows)]
mod front {
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, EnumWindows, GW_OWNER, GWL_EXSTYLE, GetForegroundWindow, GetWindow,
        GetWindowLongPtrW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SW_RESTORE,
        SetForegroundWindow, ShowWindow, WS_EX_TOOLWINDOW,
    };
    use windows_sys::core::BOOL;

    /// How long a program has to show its window.  A browser's cold start
    /// after a wake can take a while.
    const WAIT: Duration = Duration::from_secs(30);

    /// The windows on the desktop now: visible, top-level and not a tool
    /// window, as handles that can cross threads.
    pub fn windows() -> Vec<isize> {
        unsafe extern "system" fn each(w: HWND, found: LPARAM) -> BOOL {
            // SAFETY: `found` is the Vec `windows` passes in, for the length
            // of the call; `w` is a window EnumWindows hands over.
            unsafe {
                let found = &mut *(found as *mut Vec<isize>);
                let tool = GetWindowLongPtrW(w, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0;
                if IsWindowVisible(w) != 0 && GetWindow(w, GW_OWNER).is_null() && !tool {
                    found.push(w as isize);
                }
            }
            1
        }
        let mut found: Vec<isize> = Vec::new();
        // SAFETY: the callback only pushes onto `found`, which outlives the
        // call.
        unsafe { EnumWindows(Some(each), &mut found as *mut Vec<isize> as LPARAM) };
        found
    }

    /// Bring forward the first window, not among `before`, of the program
    /// `process` is running: from that process, or from another of the same
    /// executable, since a browser's launcher hands over to a process of its
    /// own and exits.  The wait is on a thread of its own; `process` is
    /// closed here.
    pub fn follow(process: HANDLE, before: Vec<isize>) {
        let program = image(process);
        // SAFETY: the handle ShellExecuteEx gave us, closed once.
        unsafe { CloseHandle(process) };
        let Some(program) = program else { return };
        std::thread::spawn(move || {
            let deadline = Instant::now() + WAIT;
            while Instant::now() < deadline {
                let new = windows()
                    .into_iter()
                    .find(|w| !before.contains(w) && window_image(*w).as_ref() == Some(&program));
                if let Some(w) = new {
                    activate(w as HWND);
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
    }

    /// The executable a process runs, in lower case for comparing.
    fn image(process: HANDLE) -> Option<String> {
        let mut buf = vec![0u16; 32 * 1024];
        let mut len = buf.len() as u32;
        // SAFETY: `buf` holds `len` characters, and the API writes no more.
        let ok = unsafe {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }

    /// The executable of the process a window belongs to.
    fn window_image(w: isize) -> Option<String> {
        let mut pid = 0u32;
        // SAFETY: `w` is a window handle; a stale one only yields no pid.
        unsafe { GetWindowThreadProcessId(w as HWND, &mut pid) };
        if pid == 0 {
            return None;
        }
        // SAFETY: the process is opened to read its name and closed again.
        unsafe {
            let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if p.is_null() {
                return None;
            }
            let name = image(p);
            CloseHandle(p);
            name
        }
    }

    /// Put `w` in front.  Joined to the input of the thread whose window is
    /// in front now, this thread is allowed to replace it.
    fn activate(w: HWND) {
        // SAFETY: plain calls on window and thread ids; the input is
        // detached again before returning.
        unsafe {
            if IsIconic(w) != 0 {
                ShowWindow(w, SW_RESTORE);
            }
            let theirs = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
            let ours = GetCurrentThreadId();
            let joined = theirs != 0 && theirs != ours && AttachThreadInput(ours, theirs, 1) != 0;
            BringWindowToTop(w);
            SetForegroundWindow(w);
            if joined {
                AttachThreadInput(ours, theirs, 0);
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open(
    target: &str,
    with: Option<&str>,
    env: &BTreeMap<String, String>,
    out: &Output,
) -> Result<Started, String> {
    let mut c = std::process::Command::new(with.unwrap_or("xdg-open"));
    c.arg(target);
    spawn(c, env, out, with.unwrap_or("xdg-open"))
}

/// Open a text file for editing: the default text editor on macOS, the
/// file's own application on Windows (Notepad if it has none).
pub fn edit(path: &std::path::Path) -> Result<(), String> {
    let target = path.to_string_lossy().into_owned();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open")
        .arg("-t")
        .arg(&target)
        .spawn()
        .map(drop);
    #[cfg(windows)]
    let r = match open(&target, None, &BTreeMap::new(), &Output::Inherit) {
        Ok(_) => Ok(()),
        Err(_) => std::process::Command::new("notepad.exe")
            .arg(&target)
            .spawn()
            .map(drop),
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    let r = std::process::Command::new("xdg-open")
        .arg(&target)
        .spawn()
        .map(drop);
    r.map_err(|e| format!("could not open {target}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn exited(code: i32) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exited(code: u32) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code)
    }

    #[test]
    fn an_exit_is_told_the_same_on_every_os() {
        assert_eq!(ended(exited(0)), "exit status: 0");
        assert_eq!(ended(exited(4)), "exit status: 4");
    }

    #[cfg(windows)]
    #[test]
    fn a_windows_exception_is_told_in_hex() {
        assert_eq!(ended(exited(0xC000_0005)), "exit status: 0xc0000005");
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_the_configured_environment_and_logs() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let env = BTreeMap::from([("SHIODOKI_TEST".to_string(), "carried".to_string())]);
        let cmd = Command::Run {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "echo $SHIODOKI_TEST; echo oops >&2; exit 3".into(),
            ],
            window: false,
        };
        let Started::Child(mut child) = start(&cmd, &env, &Output::Log(log.clone())).unwrap()
        else {
            panic!("a process")
        };
        assert_eq!(child.wait().unwrap().code(), Some(3));
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "carried\noops\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_check_answers_with_its_exit_or_not_at_all() {
        let sh = |script: &str| -> Vec<String> { vec!["sh".into(), "-c".into(), script.into()] };
        let env = BTreeMap::from([("SHIODOKI_TEST".to_string(), "3".to_string())]);
        let limit = Duration::from_secs(5);
        let code = |argv: &[String]| check(argv, &env, &Output::Discard, limit).unwrap().code();
        assert_eq!(code(&sh("echo noise; exit 0")), Some(0));
        assert_eq!(code(&sh("exit $SHIODOKI_TEST")), Some(3));
        let slow = check(
            &sh("sleep 5"),
            &env,
            &Output::Discard,
            Duration::from_millis(200),
        );
        assert_eq!(slow.unwrap_err(), "no answer within 200ms");
        let missing = check(
            &["/nonexistent/check".to_string()],
            &env,
            &Output::Discard,
            limit,
        );
        assert!(missing.unwrap_err().starts_with("/nonexistent/check:"));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_is_an_error() {
        let cmd = Command::Run {
            argv: vec!["/nonexistent/program".into()],
            window: false,
        };
        let err = start(&cmd, &BTreeMap::new(), &Output::Inherit)
            .err()
            .unwrap();
        assert!(err.starts_with("/nonexistent/program:"), "{err}");
    }
}
