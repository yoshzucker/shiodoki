//! Starting what a rule runs.
//!
//! `run` is a process started directly, with no shell.  `open` is handed
//! to the OS: `open` on macOS, `ShellExecuteW` on Windows, `xdg-open`
//! elsewhere.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::{Child, Stdio};

use crate::config::Command;

/// Where a started program's output goes.
pub enum Output {
    /// This process's own: for `shiodoki try`, run from a terminal.
    Inherit,
    /// Appended to a log file: for the agent.
    Log(PathBuf),
}

pub enum Started {
    /// A process to wait for, for its exit status.
    Child(Child),
    /// Handed to the OS, with nothing to wait for.
    HandedOff,
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

fn spawn(
    mut c: std::process::Command,
    env: &BTreeMap<String, String>,
    out: &Output,
    name: &str,
) -> Result<Started, String> {
    c.envs(env).stdin(Stdio::null());
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        c.current_dir(home);
    }
    if let Output::Log(path) = out {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let log2 = log.try_clone().map_err(|e| e.to_string())?;
        c.stdout(log).stderr(log2);
    }
    c.spawn()
        .map(Started::Child)
        .map_err(|e| format!("{name}: {e}"))
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
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
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
    // SAFETY: every pointer is to a NUL-terminated buffer that outlives the
    // call, or null where the API allows it.
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // Anything above 32 is success; below is an error code.
    if r as isize > 32 {
        Ok(Started::HandedOff)
    } else {
        Err(format!(
            "could not open {target:?}{} (ShellExecute error {})",
            with.map_or(String::new(), |w| format!(" with {w}")),
            r as isize
        ))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
