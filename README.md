# shiodoki

Run a thing while its moment lasts, and let it go once the moment has passed.

*Shiodoki* (潮時) is the turn of the tide: the moment to act, and -- as the
word is more often used -- the moment after which there is no point.

> **Status:** runs on macOS. The Windows side builds, but has not been run
> on Windows yet.

## Why

cron, launchd and Task Scheduler know *when*. They do not know whether
anybody is there. A page opened at 10:00 behind a locked screen is a page
nobody sees; a missed run is either lost, or made up for later with no idea
of *too late* -- the meeting link that opens at 11:40 for a meeting that
ended at 11:00 is worse than nothing.

shiodoki is a small resident agent for macOS and Windows that runs commands
at the first moment they can be of use, and drops them once they cannot.
Opening a page is the common case, but a command is a command: capturing a
note, syncing a folder, mounting a share.

## Words

- A **rule** says what to run and when it may run.
- A **period** is a stretch of time a rule may run in: one occurrence of its
  schedule, from `at` to `until`. A rule with no schedule has one period a
  day.
- A rule becomes **due** when its period opens (a *timed* rule), or when an
  event it listens for arrives inside its period (an *event* rule).
- A due rule **runs** at the first moment it is **clear**: the session is
  unlocked, and nothing pauses or blocks it. If the period closes first, the
  rule **lapses** for that period.

Everything else follows from those four. Waiting for an unlock, giving up
after a deadline, running once on the first login of the day: they are all a
rule that is due, waiting to be clear, inside a period that ends.

## A configuration

One file is normally one machine's, so a real one would not mix macOS
applications with Windows paths as this one does.

```toml
day_starts = "04:00"          # a late night still belongs to the day before

[env]
PATH = "/opt/homebrew/bin:/usr/bin:/bin"

# Timed: opens at 10:00 on Tuesdays if the screen is unlocked; if it is
# locked, opens on unlock -- unless that is after 10:20.
[rule.weekly-review]
every = "FREQ=WEEKLY;BYDAY=TU"
at    = "10:00"
until = "20m"
open  = "https://meet.example.com/weekly-review"
with  = "Firefox"

# Every other Thursday, counted from the first one.
[rule.retro]
every = "FREQ=WEEKLY;INTERVAL=2;BYDAY=TH"
from  = 2026-10-08
at    = "15:00"
until = "15:15"
open  = "https://meet.example.com/retro"

# Event: every login.
[rule.mount-shares]
on  = ["login"]
run = ["/usr/local/bin/mount-shares"]

# Event, once: the first time you sit down on a weekday morning, however you
# arrive -- a fresh login, an unlock, the lid opening.
[rule.morning-capture]
on    = ["login", "unlock", "wake"]
once  = true
every = "FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"
until = "12:00"
run   = ["emacsclient", "-c", "-e", "(org-capture nil \"t\")"]

# Event: joining one Wi-Fi network (Windows, for now).  Runs behind a
# locked screen too.
[rule.office-share]
on       = ["network"]
ssid     = ["Example-Office"]
unlocked = false
run      = ['C:\Windows\System32\net.exe', "use", "S:", '\\files.example.com\team']
```

## Rules

A rule is a `[rule.ID]` table. The ID is how commands such as `skip` name
it.

| Key | Meaning | Default |
|---|---|---|
| `every` | An RFC 5545 RRULE choosing the days periods open on -- see [Schedules](#schedules) | every day |
| `from` | The date the schedule counts from | -- |
| `at` | The time of day a period opens. A time earlier than `day_starts` is that day's late night, on the next calendar date | `day_starts` |
| `until` | When a period closes: a time of day (`"10:20"`) or a length (`"20m"`, `"2h"`). A time earlier than `at` is on the next day | the end of the day |
| `on` | The events that make the rule due: `login`, `unlock`, `wake`, `network`. Without `on` the rule is timed | -- |
| `once` | For an event rule: run at most once a period, instead of on every event | `false` |
| `ssid` | With `network`: the Wi-Fi network names to fire on | any change |
| `unlocked` | Wait for an unlocked session before running | `true` |
| `except` | Dates the rule never runs on, such as holidays | `[]` |
| `enabled` | `false` keeps a rule in the file without it running | `true` |
| `run` / `open` / `with` | What to run -- see [Running things](#running-things) | -- |

A period also ends when the rule's next one opens, so a rule is never in
two periods at once. A timed rule always runs once a period, at most.
`once` on a timed rule, `ssid` without `network`, or a key shiodoki does not
know is an error rather than something quietly ignored.

`day_starts` (default `"00:00"`) is where one day ends and the next begins:
for a rule with no `at`, for an `until` that defaults to the end of the day,
for `--today`, and for dates given to `skip` and `block`.

### Schedules

`every` takes the parts of an RRULE that choose days: `FREQ` (`DAILY`,
`WEEKLY`, `MONTHLY` or `YEARLY`), `INTERVAL`, `COUNT`, `UNTIL`, `BYMONTH`,
`BYMONTHDAY`, `BYDAY` (with a number in a monthly or yearly rule: `2TU`,
`-1FR`), `BYSETPOS` and `WKST`. The time of day is `at`, so `BYHOUR` and the
frequencies finer than a day are refused; so are `BYYEARDAY` and `BYWEEKNO`.

| `every` | Means |
|---|---|
| `FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR` | weekdays |
| `FREQ=WEEKLY;INTERVAL=2;BYDAY=TH` with `from` | every other Thursday |
| `FREQ=MONTHLY;BYDAY=2TU` | the second Tuesday of the month |
| `FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1` | the last weekday of the month |

`from` is needed when the rule counts from a first occurrence -- `INTERVAL`
above 1, or `COUNT` -- and when it leaves a day to it: `WEEKLY` without
`BYDAY`, `MONTHLY` or `YEARLY` without `BYDAY` or `BYMONTHDAY`. Unlike a
calendar's `DTSTART`, `from` is an occurrence only if the rule matches it.

### Events

| Event | When |
|---|---|
| `login` | The agent starting in a session it has not seen before. A session is known by its logon time, so the agent restarting in the same session -- after a crash, or a Quit and relaunch -- is not a login |
| `unlock` | The screen unlocked, or the session switched back to with fast user switching |
| `wake` | The system resumed from sleep. Usually a lock screen follows, and with `unlocked = true` the rule waits for it anyway, so `wake` on its own mostly matters when the machine woke without locking |
| `network` | The network settled into a different state: a Wi-Fi network joined, a cable plugged in, a tethered phone appearing. Bursts of changes are given a few seconds to settle first. With `ssid`, only moving onto one of those networks from another or from none -- another adapter coming up while the Wi-Fi stays the same is not joining it again. A login starts with no network known, so the one already joined counts |

An event that arrives while the rule cannot run -- the screen locked, a
pause -- is held, not dropped: the rule is due, and runs when it is clear,
if its period is still open. Several events held for one rule run it once.

### Time

Times are the machine's local time. A rule follows the machine across time
zones. A time that a clock change skips over is moved forward by the length
of the gap; a time it repeats is the first of the two.

shiodoki does not trust a timer to survive sleep. Each wake, unlock and
network event, and a tick each minute, is checked against the wall clock. A
period that opened while the machine was off or asleep is due when it comes
back, as long as it is still open.

A command that fails has still run: a period is not retried.

## Holding back

| | What it covers | Set with | Kept in | Effect |
|---|---|---|---|---|
| **pause** | Every rule, until a time | the icon, or `shiodoki pause` | this machine's state | postpones |
| **block** | A stretch of time, every rule or some | `shiodoki block` | `overrides.toml` | postpones |
| **skip** | Particular periods of one rule | `shiodoki skip` | `overrides.toml` | cancels |
| **except** | Particular dates of one rule, for good | the configuration | `config.toml` | cancels |

*Postpones* means a due rule waits: if its period is still open when the
pause or block ends, it runs then. With a rule open from 10:00 to 10:20, a
pause until 10:10 runs it at 10:10, and a pause until 11:00 lets it lapse.
*Cancels* means the period is gone: the rule is never due in it.

A pause belongs to the machine it was set on. Skips and blocks sit next to
the configuration and go wherever it goes: if one configuration is synced
between two machines, skipping a meeting skips it on both, and pausing one
machine for a presentation pauses only that one. Expired entries are
removed the next time shiodoki writes the file.

```sh
shiodoki pause 2h                        # or 30m, --until 15:30, --today
shiodoki resume
shiodoki skip weekly-review --next       # the next period only
shiodoki skip weekly-review 2026-10-13   # particular dates
shiodoki skip retro --until 2026-10-31   # every period up to a date
shiodoki block thu 13:00-15:00           # every rule
shiodoki block 2026-10-15 --rule retro   # one rule, the whole day
```

Dates are written `2026-10-15`, `today`, `tomorrow`, or a weekday name for
the nearest one, today included. `--next` is the period now open if the rule
has not run in it yet -- the meeting you are about to miss -- and the one
after otherwise.

`unskip` takes the same days, `--next` (the next skipped period) or `--until`;
with none of them it takes back every skip of the rule. `unblock` removes the
blocks overlapping a day or a stretch of one (`--rule` narrows it to blocks
naming exactly those rules), or every block with `--all`.

`overrides.toml` is plain TOML in local time, for reading and editing by
hand as much as for the commands:

```
[[skip]]
rule = "weekly-review"
on = 2026-10-13             # or from = and to =, both included

[[block]]
from = 2026-10-15T13:00:00
to = 2026-10-15T15:00:00
rules = ["retro"]           # leave out for every rule
```

## Running things

| Key | Meaning |
|---|---|
| `run` | A command and its arguments, as a list. No shell is involved: write `["sh", "-c", "..."]` or `["cmd", "/c", "..."]` when you want one |
| `open` | A URL or a file, handed to the OS: `open` on macOS, the shell's default handler on Windows |
| `with` | With `open`: the application to open it in. An application name on macOS (`"Firefox"`); an executable name or path on Windows (`"firefox.exe"`) |
| `window` | Windows: give a console program a window. Without it, console programs start hidden |

Keys under `[rule.ID.macos]` or `[rule.ID.windows]` replace the rule's own on
that OS, for a configuration shared between the two. A `run` or `open` there
replaces how the rule runs things as a whole -- `with` and `window` included.

Commands start detached; shiodoki does not wait for them. Exit status and
anything written to standard error go to the log.

A login item does not read your shell profile, so `PATH` is the system's
minimal one. Give full paths, or set `PATH` and anything else under `[env]`.

## Commands

| Command | What it does |
|---|---|
| `shiodoki-agent [--config PATH]` | The resident process: the icon, the events and the clock. What the login item starts. A second one in the same state directory stops at once |
| `shiodoki status` | Whether the agent is running or paused, which rules are due and waiting, and the next periods with skips and blocks applied |
| `shiodoki pause` / `resume` | See [Holding back](#holding-back) |
| `shiodoki skip` / `unskip` | See [Holding back](#holding-back) |
| `shiodoki block` / `unblock` | See [Holding back](#holding-back) |
| `shiodoki check` | Read the configuration and report what is wrong with it, without the agent |
| `shiodoki try ID` | Run a rule's command now, ignoring its schedule, to see that it works. It runs in the foreground, with its output in the terminal, and `try` waits for it to exit |
| `shiodoki fire EVENT [--ssid NAME]` | Hand the running agent an event as if the OS had sent it: `login` (as a new session), `unlock`, `wake`, `network`. It is refused when no agent is running, rather than kept for one that starts later |
| `shiodoki install` / `uninstall` | Add the login item and start the agent, or remove it and stop the agent |

The agent is a program of its own rather than a `shiodoki` subcommand
because on Windows a program is either a console program or a window
program, decided when it is built. The commands need the console to print
to; the agent must not open one at every login.

```
$ shiodoki status
agent    running (seen Tue 10-13 10:05)
pause    not paused
waiting  weekly-review    Tue 10-13 10:00-10:20
next     Thu 10-15 15:00-15:15  retro  (skipped)
         Tue 10-20 10:00-10:20  weekly-review
skips    retro            2026-10-15
```

The agent reads the configuration again whenever it or `overrides.toml`
changes. A configuration with an error is reported -- on the icon and in the
log -- and the last good one stays in force.

### The icon

The agent shows an icon in the macOS menu bar and the Windows notification
area. Its menu:

- what it is doing: *Watching*, or *Paused until 15:30*
- the next period to open
- Pause for 30 minutes / 1 hour / 2 hours / the rest of today
- Resume
- Open configuration
- Quit

Quit stops the agent until the next login. Periods still open when it comes
back are due as usual; events in between are not seen.

## Files

| | macOS | Windows |
|---|---|---|
| `config.toml`, `overrides.toml` | `~/.config/shiodoki/` (`$XDG_CONFIG_HOME`) | `%APPDATA%\shiodoki\` |
| `state.json`, `pause`, `heartbeat`, `shiodoki.log` | `~/.local/state/shiodoki/` (`$XDG_STATE_HOME`) | `%LOCALAPPDATA%\shiodoki\` |

`--config PATH`, or `SHIODOKI_CONFIG`, puts the configuration somewhere else
-- a synced folder, for one. `shiodoki install --config PATH` writes that
path into the login item, which does not see your environment.
`overrides.toml` always lives next to the configuration it overrides.
`SHIODOKI_STATE` moves the state directory in the same way.

The state is this machine's record of what ran, what is due, the pause, and
when the agent was last seen.
It is never meant to be synced. Two machines sharing a configuration each run
its rules on their own; there is no coordination between them.

## Install

From a clone:

```sh
cargo install --path .     # shiodoki and shiodoki-agent, side by side
shiodoki install
```

On macOS, `install` writes `~/Library/LaunchAgents/io.github.yoshzucker.shiodoki.plist`,
which starts the agent at login and again only if it crashes, so Quit stays
quit. On Windows it puts `shiodoki.lnk` in the Startup folder, pointing at
`shiodoki-agent.exe` where it was installed. Either way it stops an agent
already running and starts the new one, so running it again after
installing a new build is how to switch to it. On Windows a running agent
holds its executable open, so `shiodoki uninstall` -- or Quit from the icon
-- comes before `cargo install` there.

`shiodoki install --config PATH` writes that path into the login item; so
does `SHIODOKI_CONFIG`, if it is set when `install` runs.

## Platform notes

The agent does not subscribe to the OS's notifications. Every two seconds
it asks three questions -- is the screen locked, which Wi-Fi network is
this, which addresses are up -- and works out the events from how the
answers change. That keeps what each OS has to provide small, and the part
that decides the same on both. An unlock is heard within two seconds; a
network change once the answers have held for three samples.

| | macOS | Windows |
|---|---|---|
| Login item | LaunchAgent | Startup folder shortcut |
| Icon | menu bar | notification area |
| The session, for `login` | this user's console login, by its time | the session's number and logon time |
| Locked? | the current session's dictionary (`CGSSessionScreenIsLocked`) | the session's flags from the Terminal Services API |
| `wake` | the wall clock running ahead of the agent's own, which stops in sleep | the agent having been stopped for longer than a sample takes |
| `network` | the interfaces that are up, and their IPv4 addresses | the same |
| `ssid` | Not yet -- macOS tells only an app bundle granted Location Services which Wi-Fi network it is on | the WLAN API. On Windows 11 24H2 and later it needs *Let desktop apps access your location* (Settings > Privacy & security > Location), one switch for every desktop app; without it no network is known |

On Windows the agent is built as a window program, so starting it at login
opens no console, and console programs it runs start without one unless the
rule says `window = true`.

## Not yet

- Calendars as a source of periods: an `.ics` file or URL in place of
  `every`, so that a cancelled or declined event is a skip.
- `ssid` on macOS.
- Linux.
