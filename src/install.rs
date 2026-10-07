//! The login item: a LaunchAgent on macOS, a Startup shortcut on Windows.
//! Both point at `shiodoki-agent` where it was installed, beside `shiodoki`
//! itself, so installing a new build and logging in again is what picks it
//! up.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::store::{self, Paths};

pub const LABEL: &str = "io.github.yoshzucker.shiodoki";

/// `shiodoki-agent`, next to the running `shiodoki`.
pub fn agent_exe() -> Result<PathBuf, String> {
    let me = std::env::current_exe().map_err(|e| format!("cannot tell where shiodoki is: {e}"))?;
    let exe = me.with_file_name(if cfg!(windows) {
        "shiodoki-agent.exe"
    } else {
        "shiodoki-agent"
    });
    if exe.exists() {
        Ok(exe)
    } else {
        Err(format!(
            "{} is missing; install both programs together",
            exe.display()
        ))
    }
}

/// The agent's arguments: the configuration's path, when it is not the
/// default one -- a login item does not see the environment it was set up
/// from.
fn agent_args(paths: &Paths, explicit_config: bool) -> Result<Vec<String>, String> {
    if !explicit_config {
        return Ok(vec![]);
    }
    let abs = std::path::absolute(&paths.config).map_err(|e| e.to_string())?;
    Ok(vec!["--config".into(), abs.to_string_lossy().into_owned()])
}

/// Ask a running agent to stop, and wait until it has let go of its lock.
pub fn stop_running(paths: &Paths) -> Result<bool, String> {
    let lock_path = paths.state_dir.join("agent.lock");
    let Ok(lock) = std::fs::OpenOptions::new().write(true).open(&lock_path) else {
        return Ok(false);
    };
    if lock.try_lock().is_ok() {
        return Ok(false);
    }
    let name = format!(
        "{}-{}-quit.event",
        jiff::Timestamp::now().as_millisecond(),
        std::process::id()
    );
    store::write_atomic(&paths.inbox().join(name), "quit\n")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        if lock.try_lock().is_ok() {
            return Ok(true);
        }
    }
    Err("the running agent did not stop within ten seconds".into())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The LaunchAgent: started at login, started again only after a crash --
/// so that Quit stays quit until the next login -- and only in a session
/// with a screen.
pub fn launch_agent_plist(exe: &Path, args: &[String], state_dir: &Path) -> String {
    let mut program = format!("    <string>{}</string>\n", xml(&exe.to_string_lossy()));
    for a in args {
        program.push_str(&format!("    <string>{}</string>\n", xml(a)));
    }
    let err = xml(&state_dir.join("agent.err").to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{program}  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>LimitLoadToSessionType</key>
  <string>Aqua</string>
  <key>ProcessType</key>
  <string>Interactive</string>
  <key>StandardErrorPath</key>
  <string>{err}</string>
</dict>
</plist>
"#
    )
}

/// The login item, if there is one.
pub fn installed() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let p = plist_path().ok();
    #[cfg(windows)]
    let p = shortcut_path().ok();
    #[cfg(not(any(target_os = "macos", windows)))]
    let p: Option<PathBuf> = None;
    p.filter(|p| p.exists())
}

/// The templates, where there is no configuration yet, in the words of
/// `install`'s report.
#[cfg(any(target_os = "macos", windows))]
fn init_report(paths: &Paths) -> Result<String, String> {
    let mut out = String::new();
    for (path, written) in store::init(paths, &jiff::tz::TimeZone::system())? {
        if written {
            out.push_str(&format!("wrote the template to {}\n", path.display()));
        }
    }
    Ok(out)
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launchctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("/bin/launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "launchctl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(target_os = "macos")]
fn domain() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

#[cfg(target_os = "macos")]
pub fn install(paths: &Paths, explicit_config: bool) -> Result<String, String> {
    let exe = agent_exe()?;
    let plist = plist_path()?;
    store::write_atomic(
        &plist,
        &launch_agent_plist(&exe, &agent_args(paths, explicit_config)?, &paths.state_dir),
    )?;
    let _ = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]);
    stop_running(paths)?;
    let wrote = init_report(paths)?;
    launchctl(&["bootstrap", &domain(), &plist.to_string_lossy()])?;
    Ok(format!(
        "{wrote}installed {} and started the agent\n",
        plist.display()
    ))
}

#[cfg(target_os = "macos")]
pub fn uninstall(paths: &Paths) -> Result<String, String> {
    let plist = plist_path()?;
    let had = plist.exists();
    let booted_out = launchctl(&["bootout", &format!("{}/{LABEL}", domain())]).is_ok();
    let stopped = stop_running(paths)?;
    store::remove_if_present(&plist)?;
    Ok(uninstalled(had.then_some(&plist), booted_out || stopped))
}

#[cfg(windows)]
fn shortcut_path() -> Result<PathBuf, String> {
    let appdata = std::env::var_os("APPDATA").ok_or("APPDATA is not set")?;
    Ok(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs\Startup\shiodoki.lnk"))
}

/// PowerShell's single-quoted string, in which only `'` needs doubling.
#[cfg(any(windows, test))]
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The arguments as one Windows command line.
#[cfg(any(windows, test))]
fn win_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.contains([' ', '\t', '"']) {
                format!("\"{}\"", a.replace('"', "\\\""))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(windows)]
pub fn install(paths: &Paths, explicit_config: bool) -> Result<String, String> {
    let exe = agent_exe()?;
    let args = agent_args(paths, explicit_config)?;
    let lnk = shortcut_path()?;
    let dir = exe
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let script = format!(
        "$s = (New-Object -ComObject WScript.Shell).CreateShortcut({}); $s.TargetPath = {}; $s.Arguments = {}; $s.WorkingDirectory = {}; $s.Description = 'shiodoki agent'; $s.Save()",
        ps_quote(&lnk.to_string_lossy()),
        ps_quote(&exe.to_string_lossy()),
        ps_quote(&win_args(&args)),
        ps_quote(&dir),
    );
    let out = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .output()
        .map_err(|e| format!("powershell: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "could not write {}: {}",
            lnk.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    stop_running(paths)?;
    let wrote = init_report(paths)?;
    std::process::Command::new(&exe)
        .args(&args)
        .spawn()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(format!(
        "{wrote}installed {} and started the agent\n",
        lnk.display()
    ))
}

#[cfg(windows)]
pub fn uninstall(paths: &Paths) -> Result<String, String> {
    let lnk = shortcut_path()?;
    let had = lnk.exists();
    store::remove_if_present(&lnk)?;
    let stopped = stop_running(paths)?;
    Ok(uninstalled(had.then_some(&lnk), stopped))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn install(_: &Paths, _: bool) -> Result<String, String> {
    Err(
        "there is no login item on this OS yet; start shiodoki-agent from your session's autostart"
            .into(),
    )
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn uninstall(paths: &Paths) -> Result<String, String> {
    let stopped = stop_running(paths)?;
    Ok(uninstalled(None, stopped))
}

/// What `uninstall` did, and only that.
fn uninstalled(item: Option<&PathBuf>, stopped: bool) -> String {
    let mut out = String::new();
    match item {
        Some(p) => out.push_str(&format!("removed {}\n", p.display())),
        None => out.push_str("there was no login item\n"),
    }
    out.push_str(if stopped {
        "stopped the agent\n"
    } else {
        "no agent was running\n"
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_names_the_agent_and_its_configuration() {
        let p = launch_agent_plist(
            Path::new("/opt/bin/shiodoki-agent"),
            &["--config".into(), "/Users/x/Sync & Co/config.toml".into()],
            Path::new("/Users/x/.local/state/shiodoki"),
        );
        assert!(
            p.contains("<string>/opt/bin/shiodoki-agent</string>\n    <string>--config</string>"),
            "{p}"
        );
        assert!(p.contains("Sync &amp; Co"), "{p}");
        assert!(p.contains("<key>SuccessfulExit</key>\n    <false/>"), "{p}");
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
    }

    #[test]
    fn windows_quoting() {
        assert_eq!(
            ps_quote(r"C:\Users\O'Neil\x.lnk"),
            r"'C:\Users\O''Neil\x.lnk'"
        );
        assert_eq!(
            win_args(&[
                "--config".into(),
                r"C:\Users\x\One Drive\config.toml".into()
            ]),
            r#"--config "C:\Users\x\One Drive\config.toml""#
        );
    }

    #[test]
    fn stopping_when_nothing_runs() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths {
            config: dir.path().join("config.toml"),
            state_dir: dir.path().join("state"),
        };
        assert_eq!(stop_running(&paths), Ok(false));
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        std::fs::write(paths.state_dir.join("agent.lock"), "").unwrap();
        assert_eq!(stop_running(&paths), Ok(false), "a lock nobody holds");
    }
}
