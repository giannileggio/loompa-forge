//! Thin wrapper over the `tmux` CLI. Every call passes an argv, never a
//! shell string. Targets use tmux's `=` prefix for exact name matches, so a
//! window named `123` is never mistaken for window index 123.

use std::path::Path;
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

/// Only liveness is reported: tmux can mark a pane dead before it has
/// reaped the process, so its exit status isn't reliable. `lf exec` records
/// the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pane {
    pub window: String,
    pub dead: bool,
}

fn tmux(args: &[&str]) -> Result<Output> {
    Command::new("tmux")
        .args(args)
        .output()
        .context("running tmux (is it installed?)")
}

fn check(out: Output, what: &str) -> Result<Output> {
    if !out.status.success() {
        bail!(
            "tmux {what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out)
}

pub fn session_exists(session: &str) -> bool {
    tmux(&["has-session", "-t", &format!("={session}")]).is_ok_and(|o| o.status.success())
}

/// Starts `argv` in a new detached window named `window`. The window stays
/// around after the process exits (`remain-on-exit`) so its output can be
/// captured; setting that option in the same tmux invocation means even an
/// instantly-exiting process can't beat it.
pub fn spawn(session: &str, window: &str, workdir: &Path, argv: &[String]) -> Result<()> {
    let workdir = workdir.to_str().context("workdir is not valid UTF-8")?;
    let session_target = format!("={session}:");
    let window_target = format!("={session}:={window}");
    let mut args = if session_exists(session) {
        vec!["new-window", "-d", "-t", &session_target]
    } else {
        vec!["new-session", "-d", "-s", session]
    };
    args.extend(["-n", window, "-c", workdir]);
    let argv: Vec<String> = argv.iter().map(|a| escape_arg(a)).collect();
    args.extend(argv.iter().map(String::as_str));
    args.extend([
        ";",
        "set-option",
        "-w",
        "-t",
        &window_target,
        "remain-on-exit",
        "on",
    ]);
    check(tmux(&args)?, "spawn")?;
    Ok(())
}

/// tmux treats an argument ending in `;` as a command separator and turns a
/// trailing `\;` into a literal `;`.
fn escape_arg(arg: &str) -> String {
    match arg.strip_suffix(';') {
        Some(rest) => format!("{rest}\\;"),
        None => arg.to_string(),
    }
}

/// Every pane in `session`. A missing session yields no panes.
pub fn panes(session: &str) -> Result<Vec<Pane>> {
    if !session_exists(session) {
        return Ok(Vec::new());
    }
    let out = check(
        tmux(&[
            "list-panes",
            "-s",
            "-t",
            &format!("={session}"),
            "-F",
            "#{pane_dead} #{window_name}",
        ])?,
        "list-panes",
    )?;
    Ok(parse_panes(&String::from_utf8_lossy(&out.stdout)))
}

fn parse_panes(text: &str) -> Vec<Pane> {
    text.lines()
        .filter_map(|line| {
            let (dead, window) = line.split_once(' ')?;
            Some(Pane {
                window: window.to_string(),
                dead: dead == "1",
            })
        })
        .collect()
}

/// The pane's full scrollback, for the task log.
pub fn capture(session: &str, window: &str) -> Result<String> {
    let out = check(
        tmux(&[
            "capture-pane",
            "-p",
            "-J",
            "-S",
            "-",
            "-t",
            &format!("={session}:={window}"),
        ])?,
        "capture-pane",
    )?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Kills the window. A window that's already gone is not an error.
pub fn kill_window(session: &str, window: &str) -> Result<()> {
    let out = tmux(&["kill-window", "-t", &format!("={session}:={window}")])?;
    let err = String::from_utf8_lossy(&out.stderr);
    if out.status.success() || err.contains("can't find") || err.contains("no server") {
        Ok(())
    } else {
        bail!("tmux kill-window failed: {}", err.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn parses_list_panes_output() {
        let panes = parse_panes("0 running-task\n1 has space\n");
        assert_eq!(
            panes,
            vec![
                Pane {
                    window: "running-task".into(),
                    dead: false
                },
                Pane {
                    window: "has space".into(),
                    dead: true
                },
            ]
        );
    }

    struct Session(String);
    impl Drop for Session {
        fn drop(&mut self) {
            let _ = tmux(&["kill-session", "-t", &format!("={}", self.0)]);
        }
    }

    fn wait_dead(session: &str, window: &str) -> Pane {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(p) = panes(session)
                .unwrap()
                .into_iter()
                .find(|p| p.window == window)
                && p.dead
            {
                return p;
            }
            assert!(Instant::now() < deadline, "{window} never exited");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn finished_windows_stay_until_killed() {
        let s = Session(format!("lf-test-{}", std::process::id()));
        let dir = std::env::temp_dir();
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        // First spawn creates the session, second adds a window to it.
        spawn(&s.0, "ok", &dir, &argv(&["true"])).unwrap();
        spawn(&s.0, "123", &dir, &argv(&["echo", "semi;", "back\\;"])).unwrap();

        wait_dead(&s.0, "ok");
        wait_dead(&s.0, "123"); // found by name, not as window index 123
        assert!(capture(&s.0, "123").unwrap().contains("semi; back\\;"));

        kill_window(&s.0, "ok").unwrap();
        kill_window(&s.0, "ok").unwrap(); // already gone: still fine
        assert!(panes(&s.0).unwrap().iter().all(|p| p.window != "ok"));
    }
}
