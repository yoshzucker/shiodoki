//! Where the files live, and reading and writing them.
//!
//! | File | Written by | Read by |
//! |---|---|---|
//! | `config.toml` | a person | everything |
//! | `overrides.toml` | `skip`, `block` (and a person) | the agent, `status` |
//! | `pause` | `pause`, `resume`, the icon | the agent, `status` |
//! | `state.json` | the agent | `status`, `skip --next` |
//! | `heartbeat` | the agent, each tick | `status` |
//! | `inbox/*.event` | `fire` | the agent, which removes them |
//!
//! Every write goes to a temporary file that is then renamed over the old
//! one, so a reader never sees half of one.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use crate::config::{Config, ConfigError, Os};
use crate::engine::State;
use crate::overrides::Overrides;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub config: PathBuf,
    pub state_dir: PathBuf,
}

impl Paths {
    /// `--config`, else `SHIODOKI_CONFIG`, else the OS's configuration
    /// directory.  The state directory is `SHIODOKI_STATE`, else the OS's.
    pub fn resolve(config: Option<PathBuf>) -> Result<Paths, String> {
        let config = match config.or_else(|| env_path("SHIODOKI_CONFIG")) {
            Some(p) => p,
            None => default_config_dir()?.join("config.toml"),
        };
        let state_dir = match env_path("SHIODOKI_STATE") {
            Some(p) => p,
            None => default_state_dir()?,
        };
        Ok(Paths { config, state_dir })
    }

    /// Next to the configuration it overrides, wherever that is.
    pub fn overrides(&self) -> PathBuf {
        self.config.with_file_name("overrides.toml")
    }

    pub fn state(&self) -> PathBuf {
        self.state_dir.join("state.json")
    }

    pub fn pause(&self) -> PathBuf {
        self.state_dir.join("pause")
    }

    pub fn heartbeat(&self) -> PathBuf {
        self.state_dir.join("heartbeat")
    }

    /// Where `shiodoki fire` leaves events for the agent.
    pub fn inbox(&self) -> PathBuf {
        self.state_dir.join("inbox")
    }

    pub fn log(&self) -> PathBuf {
        self.state_dir.join("shiodoki.log")
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// On Windows, USERPROFILE: HOME there is whatever a POSIX shell such as
/// MSYS2's set it to -- `/home/you` -- and the agent, started at login, does
/// not see it.
fn home() -> Result<PathBuf, String> {
    env_path(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok_or_else(|| "cannot tell where the home directory is".to_string())
}

/// An XDG base directory, ignored unless it is absolute, as the
/// specification says.  On Windows that leaves out a POSIX path.
fn xdg(name: &str) -> Option<PathBuf> {
    env_path(name).filter(|p| p.is_absolute())
}

/// `~/.config/shiodoki` on Windows too: the configuration is a file a person
/// edits, and keeps with the rest of their dotfiles.
fn default_config_dir() -> Result<PathBuf, String> {
    let base = xdg("XDG_CONFIG_HOME").map_or_else(|| home().map(|h| h.join(".config")), Ok)?;
    Ok(base.join("shiodoki"))
}

/// The state is this machine's alone, so on Windows it stays in
/// LOCALAPPDATA, which a roaming profile leaves behind.
fn default_state_dir() -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        env_path("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?
    } else {
        xdg("XDG_STATE_HOME").map_or_else(|| home().map(|h| h.join(".local").join("state")), Ok)?
    };
    Ok(base.join("shiodoki"))
}

/// The file's contents, or `None` if there is no such file.
pub fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(fail)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, contents).map_err(fail)?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        fail(e)
    })
}

pub fn remove_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

#[derive(Debug)]
pub enum ConfigLoad {
    Missing(PathBuf),
    Unreadable(String),
    Invalid(PathBuf, ConfigError),
}

impl std::fmt::Display for ConfigLoad {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigLoad::Missing(p) => write!(f, "no configuration at {}", p.display()),
            ConfigLoad::Unreadable(e) => write!(f, "{e}"),
            ConfigLoad::Invalid(p, e) => write!(f, "{}:\n{e}", p.display()),
        }
    }
}

pub fn load_config(paths: &Paths, os: Os) -> Result<Config, ConfigLoad> {
    let text = read_optional(&paths.config)
        .map_err(ConfigLoad::Unreadable)?
        .ok_or_else(|| ConfigLoad::Missing(paths.config.clone()))?;
    Config::parse(&text, os).map_err(|e| ConfigLoad::Invalid(paths.config.clone(), e))
}

pub fn load_overrides(paths: &Paths, tz: &TimeZone) -> Result<Overrides, String> {
    let path = paths.overrides();
    match read_optional(&path)? {
        None => Ok(Overrides::default()),
        Some(text) => Overrides::parse(&text, tz)
            .map_err(|ps| format!("{}:\n{}", path.display(), ps.join("\n"))),
    }
}

/// A configuration with every setting there is, commented out.
pub const CONFIG_TEMPLATE: &str = include_str!("../templates/config.toml");
/// The head of every `overrides.toml` written: what it holds, in examples.
pub const OVERRIDES_TEMPLATE: &str = include_str!("../templates/overrides.toml");

/// The commented examples, and the skips and blocks after them.  A file
/// with none of either keeps the examples, as a reference for editing.
pub fn save_overrides(paths: &Paths, o: &Overrides, tz: &TimeZone) -> Result<(), String> {
    write_atomic(
        &paths.overrides(),
        &format!("{OVERRIDES_TEMPLATE}{}", o.to_toml(tz)),
    )
}

/// Write the templates where there is no file yet; never over one.  Each
/// path, and whether it was written.
pub fn init(paths: &Paths, tz: &TimeZone) -> Result<Vec<(PathBuf, bool)>, String> {
    let mut out = vec![];
    if paths.config.exists() {
        out.push((paths.config.clone(), false));
    } else {
        write_atomic(&paths.config, CONFIG_TEMPLATE)?;
        out.push((paths.config.clone(), true));
    }
    let o = paths.overrides();
    if o.exists() {
        out.push((o, false));
    } else {
        save_overrides(paths, &Overrides::default(), tz)?;
        out.push((o, true));
    }
    Ok(out)
}

fn read_timestamp(path: &Path) -> Result<Option<Timestamp>, String> {
    match read_optional(path)? {
        None => Ok(None),
        Some(s) => s
            .trim()
            .parse::<Timestamp>()
            .map(Some)
            .map_err(|_| format!("{}: {:?} is not a timestamp", path.display(), s.trim())),
    }
}

/// Until when this machine is paused; `None` when it is not, or no longer.
pub fn read_pause(paths: &Paths, now: Timestamp) -> Result<Option<Timestamp>, String> {
    Ok(read_timestamp(&paths.pause())?.filter(|until| *until > now))
}

pub fn write_pause(paths: &Paths, until: Option<Timestamp>, tz: &TimeZone) -> Result<(), String> {
    match until {
        None => remove_if_present(&paths.pause()),
        Some(t) => write_atomic(&paths.pause(), &format!("{}\n", local_rfc3339(t, tz))),
    }
}

/// `2026-10-13T12:00:00+09:00`: an instant a person can read as local time.
pub fn local_rfc3339(t: Timestamp, tz: &TimeZone) -> String {
    t.to_zoned(tz.clone())
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

pub fn load_state(paths: &Paths) -> Result<State, String> {
    let path = paths.state();
    match read_optional(&path)? {
        None => Ok(State::default()),
        Some(text) => serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
    }
}

pub fn save_state(paths: &Paths, state: &State) -> Result<(), String> {
    let text = serde_json::to_string_pretty(state).expect("a state serializes");
    write_atomic(&paths.state(), &format!("{text}\n"))
}

pub fn read_heartbeat(paths: &Paths) -> Result<Option<Timestamp>, String> {
    read_timestamp(&paths.heartbeat())
}

pub fn write_heartbeat(paths: &Paths, now: Timestamp) -> Result<(), String> {
    write_atomic(&paths.heartbeat(), &format!("{now}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overrides::Skip;
    use jiff::civil::date;

    fn paths(dir: &Path) -> Paths {
        Paths {
            config: dir.join("conf").join("config.toml"),
            state_dir: dir.join("state"),
        }
    }

    /// The template with every example taken out of its comment: a `#`
    /// followed at once by something other than a space or another `#`.
    fn uncommented(template: &str) -> String {
        template
            .lines()
            .map(|l| match l.strip_prefix('#') {
                Some(rest) if !rest.is_empty() && !rest.starts_with([' ', '#']) => rest,
                _ => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_templates_read_as_they_are_and_with_every_example_in_use() {
        let tz = TimeZone::fixed(jiff::tz::offset(9));
        for os in [Os::MacOS, Os::Windows] {
            assert!(Config::parse(CONFIG_TEMPLATE, os).unwrap().rules.is_empty());
            let all =
                Config::parse(&uncommented(CONFIG_TEMPLATE), os).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                all.rules.len(),
                14,
                "{:?}",
                all.rules.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(
            Overrides::parse(OVERRIDES_TEMPLATE, &tz).unwrap(),
            Overrides::default()
        );
        let o = Overrides::parse(&uncommented(OVERRIDES_TEMPLATE), &tz).unwrap();
        assert_eq!((o.skips.len(), o.blocks.len()), (2, 2));
    }

    #[test]
    fn init_writes_only_what_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let tz = TimeZone::UTC;
        let written: Vec<bool> = init(&p, &tz).unwrap().into_iter().map(|(_, w)| w).collect();
        assert_eq!(written, [true, true]);
        assert_eq!(fs::read_to_string(&p.config).unwrap(), CONFIG_TEMPLATE);
        fs::write(&p.config, "[rule.mine]\nrun = [\"x\"]\n").unwrap();
        let written: Vec<bool> = init(&p, &tz).unwrap().into_iter().map(|(_, w)| w).collect();
        assert_eq!(written, [false, false]);
        assert!(
            fs::read_to_string(&p.config).unwrap().contains("rule.mine"),
            "never over a file"
        );
    }

    #[test]
    fn missing_files_are_empty_not_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let tz = TimeZone::UTC;
        assert!(matches!(
            load_config(&p, Os::MacOS),
            Err(ConfigLoad::Missing(_))
        ));
        assert_eq!(load_overrides(&p, &tz).unwrap(), Overrides::default());
        assert_eq!(read_pause(&p, Timestamp::UNIX_EPOCH).unwrap(), None);
        assert_eq!(load_state(&p).unwrap(), State::default());
        assert_eq!(read_heartbeat(&p).unwrap(), None);
    }

    #[test]
    fn round_trips_and_keeps_the_examples() {
        let dir = tempfile::tempdir().unwrap();
        let p = paths(dir.path());
        let tz = TimeZone::fixed(jiff::tz::offset(9));
        let mut o = Overrides::default();
        o.skips.push(Skip {
            rule: "a".into(),
            from: date(2026, 10, 13),
            to: date(2026, 10, 13),
        });
        save_overrides(&p, &o, &tz).unwrap();
        assert!(p.overrides().starts_with(dir.path().join("conf")));
        assert_eq!(load_overrides(&p, &tz).unwrap(), o);
        save_overrides(&p, &Overrides::default(), &tz).unwrap();
        let kept = fs::read_to_string(p.overrides()).unwrap();
        assert_eq!(kept, OVERRIDES_TEMPLATE, "only the examples are left");
        assert_eq!(load_overrides(&p, &tz).unwrap(), Overrides::default());

        let now: Timestamp = "2026-10-13T01:00:00Z".parse().unwrap();
        let later: Timestamp = "2026-10-13T03:00:00Z".parse().unwrap();
        write_pause(&p, Some(later), &tz).unwrap();
        assert!(fs::read_to_string(p.pause()).unwrap().contains("+09:00"));
        assert_eq!(read_pause(&p, now).unwrap(), Some(later));
        assert_eq!(
            read_pause(&p, later).unwrap(),
            None,
            "a pause that has ended is no pause"
        );
        write_pause(&p, None, &tz).unwrap();
        assert!(!p.pause().exists());

        write_heartbeat(&p, now).unwrap();
        assert_eq!(read_heartbeat(&p).unwrap(), Some(now));
    }
}
