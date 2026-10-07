//! `shiodoki-agent`: the resident process.
//!
//! A window program on Windows, so that starting it at login opens no
//! console.  The work is a thread of its own that samples the OS every two
//! seconds and hands the samples to [`Agent`]; the main thread belongs to
//! the icon, because both macOS and Windows want their UI there.

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use jiff::tz::TimeZone;

use shiodoki::agent::{Agent, MenuAction, View};
use shiodoki::config::Os;
use shiodoki::platform;
use shiodoki::store::Paths;
use shiodoki::watch::Sample;

const INTERVAL: Duration = Duration::from_secs(2);

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut config = None;
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--config") => config = args.next().map(PathBuf::from),
            Some("--version") => {
                println!("shiodoki-agent {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => {
                eprintln!("usage: shiodoki-agent [--config PATH]");
                std::process::exit(2);
            }
        }
    }
    let paths = match Paths::resolve(config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("shiodoki-agent: {e}");
            std::process::exit(1);
        }
    };
    let agent = Agent::new(paths, Os::current(), TimeZone::system());

    // One agent per state directory: a second would run every rule twice.
    let _ = std::fs::create_dir_all(&agent.paths().state_dir);
    let lock = std::fs::File::create(agent.paths().state_dir.join("agent.lock"));
    let _lock = match lock {
        Ok(f) if f.try_lock().is_ok() => f,
        _ => {
            agent.log("another agent is already running; this one stops");
            return;
        }
    };
    let log = agent.paths().log();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
        {
            use std::io::Write as _;
            let _ = writeln!(f, "{} panic: {info}", Timestamp::now());
        }
    }));

    ui::run(agent);
}

/// The work: sample, decide, run, and tell the icon.
fn work(mut agent: Agent, menu: Receiver<MenuAction>, show: impl Fn(View)) {
    let started = Instant::now();
    let sample = || Sample {
        wall: Timestamp::now(),
        mono: started.elapsed(),
        locked: platform::is_locked(),
        net: platform::net_state(),
    };
    let launches = agent.begin(sample(), platform::session_id());
    agent.run(launches);
    let mut shown = None;
    loop {
        let view = agent.view(Timestamp::now());
        if shown.as_ref() != Some(&view) {
            show(view.clone());
            shown = Some(view);
        }
        match menu.recv_timeout(INTERVAL) {
            Ok(MenuAction::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(action) => agent.menu(action, Timestamp::now()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        let launches = agent.tick(sample());
        agent.run(launches);
        if agent.wants_quit() {
            break;
        }
    }
    agent.menu(MenuAction::Quit, Timestamp::now());
}

#[cfg(any(target_os = "macos", windows))]
mod ui {
    use std::sync::mpsc;

    use jiff::SignedDuration;
    use tao::event::{Event, StartCause};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};

    use super::{Agent, MenuAction, View, work};

    enum UserEvent {
        View(View),
        Quit,
    }

    pub fn run(agent: Agent) {
        #[allow(unused_mut)]
        let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        #[cfg(target_os = "macos")]
        {
            use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
            event_loop.set_activation_policy(ActivationPolicy::Accessory);
        }

        let status = MenuItem::new("Starting", false, None);
        let next = MenuItem::new("", false, None);
        let pauses = [
            (
                "Pause for 30 minutes",
                MenuAction::Pause(SignedDuration::from_mins(30)),
            ),
            (
                "Pause for 1 hour",
                MenuAction::Pause(SignedDuration::from_hours(1)),
            ),
            (
                "Pause for 2 hours",
                MenuAction::Pause(SignedDuration::from_hours(2)),
            ),
            ("Pause for the rest of today", MenuAction::PauseToday),
        ]
        .map(|(text, action)| (MenuItem::new(text, true, None), action));
        let resume = MenuItem::new("Resume", false, None);
        let open = MenuItem::new("Open configuration", true, None);
        let quit = MenuItem::new("Quit shiodoki", true, None);

        let menu = Menu::new();
        let _ = menu.append_items(&[&status, &next, &PredefinedMenuItem::separator()]);
        for (item, _) in &pauses {
            let _ = menu.append(item);
        }
        let _ = menu.append_items(&[&resume, &PredefinedMenuItem::separator(), &open, &quit]);

        let mut actions: Vec<_> = pauses.iter().map(|(i, a)| (i.id().clone(), *a)).collect();
        actions.push((resume.id().clone(), MenuAction::Resume));
        actions.push((open.id().clone(), MenuAction::OpenConfig));
        actions.push((quit.id().clone(), MenuAction::Quit));
        let (tx, rx) = mpsc::channel();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some((_, a)) = actions.iter().find(|(id, _)| *id == e.id) {
                let _ = tx.send(*a);
            }
        }));

        let proxy = event_loop.create_proxy();
        let views = proxy.clone();
        std::thread::spawn(move || {
            work(agent, rx, move |v| {
                let _ = views.send_event(UserEvent::View(v));
            });
            let _ = proxy.send_event(UserEvent::Quit);
        });

        let mut tray = None;
        let mut dim = None;
        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            match event {
                // The icon is made once the loop runs, not before, as macOS
                // asks.
                Event::NewEvents(StartCause::Init) => {
                    let builder = TrayIconBuilder::new()
                        .with_menu(Box::new(menu.clone()))
                        .with_tooltip("shiodoki");
                    // macOS draws a template in the menu bar's own colour,
                    // from its alpha alone.
                    #[cfg(target_os = "macos")]
                    let builder = builder.with_icon_templated(icon(false));
                    #[cfg(windows)]
                    let builder = builder.with_icon(icon(false));
                    tray = builder.build().ok();
                }
                Event::UserEvent(UserEvent::View(v)) => {
                    status.set_text(&v.headline);
                    next.set_text(v.next.as_deref().unwrap_or("Nothing timed ahead"));
                    resume.set_enabled(v.paused);
                    if let Some(t) = &tray {
                        let _ = t.set_tooltip(Some(format!("shiodoki: {}", v.headline)));
                        let want = v.paused || v.problem;
                        if dim != Some(want) {
                            #[cfg(target_os = "macos")]
                            let _ = t.set_icon_templated(Some(icon(want)));
                            #[cfg(windows)]
                            let _ = t.set_icon(Some(icon(want)));
                            dim = Some(want);
                        }
                    }
                }
                Event::UserEvent(UserEvent::Quit) => {
                    tray = None;
                    *control_flow = ControlFlow::ExitWithCode(0);
                }
                _ => {}
            }
        });
    }

    /// A ring with water in it -- and an empty one when held back.  Black on
    /// macOS, which draws it as a template in the menu bar's own colour; a
    /// blue that reads on light and dark taskbars on Windows.
    fn icon(empty: bool) -> Icon {
        let n: u32 = if cfg!(target_os = "macos") { 36 } else { 32 };
        let (r, g, b) = if cfg!(target_os = "macos") {
            (0, 0, 0)
        } else {
            (0x3b, 0x82, 0xc4)
        };
        let size = n as f32;
        let c = (size - 1.0) / 2.0;
        let outer = size * 0.46;
        let inner = outer - size * 0.10;
        let well = inner - size * 0.07;
        let mut rgba = Vec::with_capacity((n * n * 4) as usize);
        for y in 0..n {
            for x in 0..n {
                let (fx, fy) = (x as f32, y as f32);
                let d = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
                let ring =
                    ((outer - d + 0.5).clamp(0.0, 1.0)) * ((d - inner + 0.5).clamp(0.0, 1.0));
                let surface = c
                    + size * 0.05
                    + size * 0.05 * (fx / size * std::f32::consts::TAU * 1.25).sin();
                let water = if empty {
                    0.0
                } else {
                    ((well - d + 0.5).clamp(0.0, 1.0)) * ((fy - surface + 0.5).clamp(0.0, 1.0))
                };
                let a = ring.max(water);
                rgba.extend_from_slice(&[r, g, b, (a * 255.0).round() as u8]);
            }
        }
        Icon::from_rgba(rgba, n, n).expect("a square icon")
    }
}

/// Elsewhere there is no icon: the work runs on the main thread until it
/// is asked to stop.
#[cfg(not(any(target_os = "macos", windows)))]
mod ui {
    use super::{Agent, work};

    pub fn run(agent: Agent) {
        let (_tx, rx) = std::sync::mpsc::channel();
        work(agent, rx, |_| {});
    }
}
