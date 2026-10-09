//! `shiodoki`: the commands.  The resident process is `shiodoki-agent`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jiff::Timestamp;
use jiff::tz::TimeZone;

use shiodoki::cli::{Cli, PauseFor, Which};
use shiodoki::config::Os;
use shiodoki::store::Paths;

/// Run a thing while its moment lasts, and let it go once the moment has
/// passed.
#[derive(Parser)]
#[command(name = "shiodoki", version)]
struct Args {
    /// The configuration file [default: $SHIODOKI_CONFIG, or the OS's
    /// configuration directory]
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Read the configuration and report what is wrong with it
    Check,
    /// Whether the agent runs or is paused, what waits, and what comes next
    Status,
    /// Hold every rule back on this machine
    Pause {
        /// For this long: 30m, 1h, 2h
        #[arg(required_unless_present_any = ["until", "today"])]
        length: Option<String>,
        /// Until this time of day
        #[arg(long, value_name = "HH:MM", conflicts_with_all = ["length", "today"])]
        until: Option<String>,
        /// For the rest of the day
        #[arg(long, conflicts_with = "length")]
        today: bool,
    },
    /// End a pause
    Resume,
    /// Cancel some periods of a rule
    Skip {
        rule: String,
        #[command(flatten)]
        which: WhichArgs,
    },
    /// Take skips back; with no days, every skip of the rule
    Unskip {
        rule: String,
        #[command(flatten)]
        which: OptionalWhichArgs,
    },
    /// Hold rules back for a stretch of time
    Block {
        /// The day: 2026-10-15, today, tomorrow, or a weekday
        when: String,
        /// The times, such as 13:00-15:00 [default: the whole day]
        range: Option<String>,
        /// Only this rule (may be given more than once) [default: every rule]
        #[arg(long = "rule", value_name = "RULE")]
        rules: Vec<String>,
    },
    /// Remove the blocks overlapping a day or a stretch of it
    Unblock {
        when: Option<String>,
        range: Option<String>,
        /// Only the blocks naming exactly these rules
        #[arg(long = "rule", value_name = "RULE")]
        rules: Vec<String>,
        /// Every block
        #[arg(long, conflicts_with_all = ["when", "range"])]
        all: bool,
    },
    /// Run a rule's command now, ignoring its schedule
    Try {
        rule: String,
        /// Have the running agent run it, in its own environment, and say
        /// how it went: the whole way from the agent to the command
        #[arg(long)]
        agent: bool,
    },
    /// Write the commented templates where there are no files yet
    Init,
    /// Hand the running agent an event, as if the OS had sent it
    Fire {
        /// login, unlock, wake or network
        event: String,
        /// With network: the Wi-Fi network to be on, until the real one changes
        #[arg(long, value_name = "NAME")]
        ssid: Option<String>,
    },
    /// Start the agent at every login, and now
    Install,
    /// Stop starting the agent at login, and stop it
    Uninstall,
}

#[derive(clap::Args)]
#[group(required = true, multiple = false)]
struct WhichArgs {
    /// Days: 2026-10-15, today, tomorrow, or a weekday
    days: Vec<String>,
    /// The period now open if the rule has not run in it, else the next
    #[arg(long)]
    next: bool,
    /// Every period from today up to and including this day
    #[arg(long, value_name = "DAY")]
    until: Option<String>,
}

#[derive(clap::Args)]
#[group(required = false, multiple = false)]
struct OptionalWhichArgs {
    /// Days: 2026-10-15, today, tomorrow, or a weekday
    days: Vec<String>,
    /// The next skipped period
    #[arg(long)]
    next: bool,
    /// Every skipped period from today up to and including this day
    #[arg(long, value_name = "DAY")]
    until: Option<String>,
}

fn which(days: Vec<String>, next: bool, until: Option<String>) -> Option<Which> {
    if next {
        Some(Which::Next)
    } else if let Some(u) = until {
        Some(Which::Until(u))
    } else if !days.is_empty() {
        Some(Which::Days(days))
    } else {
        None
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let explicit_config =
        args.config.is_some() || std::env::var_os("SHIODOKI_CONFIG").is_some_and(|v| !v.is_empty());
    let paths = match Paths::resolve(args.config) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("shiodoki: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cli = Cli {
        paths,
        now: Timestamp::now(),
        tz: TimeZone::system(),
        os: Os::current(),
    };
    let result = match args.command {
        Cmd::Check => cli.check(),
        Cmd::Status => cli.status(),
        Cmd::Pause {
            length,
            until,
            today,
        } => cli.pause(match (length, until, today) {
            (Some(l), _, _) => PauseFor::Length(l),
            (_, Some(u), _) => PauseFor::Until(u),
            _ => PauseFor::Today,
        }),
        Cmd::Resume => cli.resume(),
        Cmd::Skip { rule, which: w } => cli.skip(
            &rule,
            which(w.days, w.next, w.until).expect("clap requires one"),
        ),
        Cmd::Unskip { rule, which: w } => cli.unskip(&rule, which(w.days, w.next, w.until)),
        Cmd::Block { when, range, rules } => cli.block(&when, range.as_deref(), rules),
        Cmd::Unblock {
            when,
            range,
            rules,
            all,
        } => cli.unblock(when.as_deref(), range.as_deref(), rules, all),
        Cmd::Try { rule, agent: false } => cli.try_rule(&rule),
        Cmd::Try { rule, agent: true } => {
            cli.try_with_agent(&rule, std::time::Duration::from_secs(15))
        }
        Cmd::Init => cli.init(),
        Cmd::Fire { event, ssid } => cli.fire(&event, ssid.as_deref()),
        Cmd::Install => shiodoki::install::install(&cli.paths, explicit_config),
        Cmd::Uninstall => shiodoki::install::uninstall(&cli.paths),
    };
    match result {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("shiodoki: {e}");
            ExitCode::FAILURE
        }
    }
}
