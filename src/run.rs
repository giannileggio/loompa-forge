//! `lf run`: enqueue due schedules, poll running tasks, start pending ones.
//!
//! Each task runs in its own tmux window, named after the task id. The
//! window runs `lf exec <id>`, which loads the task, runs the agent and
//! records its exit status in `logs/<id>.<attempt>.exit`. So the prompt
//! never passes through tmux's command line (which splits arguments ending
//! in `;` and caps message size), and the outcome doesn't depend on tmux's
//! own exit-status tracking (which lags behind `pane_dead`).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Local};
use serde::{Deserialize, Serialize};

use crate::cmd::{md_files, unique_id};
use crate::config::{Agent, Config};
use crate::git;
use crate::home::Home;
use crate::schedule::Schedule;
use crate::spec::{Effective, Mode};
use crate::task::{Status, Task, now};
use crate::tmux;

pub fn run(home: &Home, once: bool) -> Result<()> {
    home.ensure_initialized()?;
    let mut config = Config::load(&home.config_path())?;
    if !once {
        log(&format!(
            "watching {} every {}",
            home.root().display(),
            humantime::format_duration(config.runner.poll_interval)
        ));
    }
    loop {
        if let Err(e) = tick(home, &config) {
            if once {
                return Err(e);
            }
            log(&format!("error: {e:#}"));
        }
        if once {
            return Ok(());
        }
        std::thread::sleep(config.runner.poll_interval);
        match Config::load(&home.config_path()) {
            Ok(c) => config = c,
            Err(e) => log(&format!("keeping the previous config: {e:#}")),
        }
    }
}

/// Runs inside the task's tmux window: runs the agent, then writes how it
/// ended to `status_file` for [`poll_running`] to pick up. If the agent's
/// (headless-only) stdout is a JSON object reporting `usage`, also writes
/// token/cost usage next to it, at [`usage_file_for`].
pub fn exec(home: &Home, id: &str, status_file: &Path) -> Result<ExitCode> {
    let spawned = (|| {
        let config = Config::load(&home.config_path())?;
        let task = Task::load(&home.tasks().join(format!("{id}.md")))?;
        let eff = task.spec.resolve(&config.defaults);
        let name = eff.agent.as_deref().context("no agent set")?;
        let agent = config
            .agents
            .get(name)
            .with_context(|| format!("unknown agent `{name}`"))?;
        let model = eff.model.as_deref().or(agent.model.as_deref());
        let argv = agent_argv(agent, eff.mode, &task.prompt, model, &task.id);
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .env("LF_HOME", home.root())
            .env("LF_TASK_ID", &task.id);
        // Headless stdout is captured (and teed back below) so a final JSON
        // summary can be parsed for usage. Interactive mode keeps a real,
        // fully inherited tty: it's a live session for a human to use.
        if eff.mode == Mode::Headless {
            cmd.stdout(Stdio::piped());
        }
        let child = cmd
            .spawn()
            .with_context(|| format!("starting `{}`", argv[0]))?;
        Ok::<_, anyhow::Error>((child, eff.mode))
    })();
    let (exit, usage) = match spawned {
        Ok((mut child, mode)) => {
            // Ctrl-C / Ctrl-\ in the window are meant for the agent; this
            // process must outlive it to record the status. Ignoring them
            // only after the spawn keeps the child's handlers default.
            // SAFETY: plain libc calls with valid constants.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_IGN);
                libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            }
            // Tee piped stdout back to our own (the tmux pane) as it
            // arrives, so `lf attach`/`lf logs` still see it live, while
            // also keeping a copy to parse for usage once the agent exits.
            let stdout_thread = (mode == Mode::Headless).then(|| {
                let mut stdout = child.stdout.take().expect("piped for headless mode");
                std::thread::spawn(move || -> Vec<u8> {
                    let mut out = std::io::stdout();
                    let mut buf = [0u8; 8192];
                    let mut captured = Vec::new();
                    while let Ok(n) = stdout.read(&mut buf)
                        && n > 0
                    {
                        let _ = out.write_all(&buf[..n]);
                        captured.extend_from_slice(&buf[..n]);
                    }
                    let _ = out.flush();
                    captured
                })
            });
            let status = child.wait().context("waiting for the agent")?;
            let captured = stdout_thread.and_then(|t| t.join().ok());
            let exit = match (status.code(), status.signal()) {
                (Some(code), _) => Exit::Code(code),
                (None, Some(sig)) => Exit::Signal(sig),
                (None, None) => Exit::Code(1),
            };
            let usage = captured.and_then(|out| Usage::parse(&out));
            (exit, usage)
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            (Exit::Code(127), None)
        }
    };
    // Written before the exit record, so a poller that sees the exit record
    // finds any usage already there.
    if let Some(usage) = usage {
        let path = usage_file_for(status_file);
        if let Err(e) = std::fs::write(&path, usage.to_json()) {
            eprintln!("warning: could not write {}: {e:#}", path.display());
        }
    }
    std::fs::write(status_file, exit.to_string())
        .with_context(|| format!("writing {}", status_file.display()))?;
    Ok(match exit {
        Exit::Code(c) => ExitCode::from(c.clamp(0, 255) as u8),
        Exit::Signal(_) => ExitCode::FAILURE,
    })
}

/// How the agent process ended, as recorded by [`exec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Code(i32),
    Signal(i32),
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Exit::Code(c) => write!(f, "exit {c}"),
            Exit::Signal(s) => write!(f, "signal {s}"),
        }
    }
}

impl std::str::FromStr for Exit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let bad = || format!("unrecognized exit record `{s}`");
        let (kind, n) = s.trim().split_once(' ').ok_or_else(bad)?;
        let n = n.parse().map_err(|_| bad())?;
        match kind {
            "exit" => Ok(Exit::Code(n)),
            "signal" => Ok(Exit::Signal(n)),
            _ => Err(bad()),
        }
    }
}

fn status_file(home: &Home, id: &str, attempt: u32) -> PathBuf {
    home.logs().join(format!("{id}.{attempt}.exit"))
}

fn usage_file(home: &Home, id: &str, attempt: u32) -> PathBuf {
    home.logs().join(format!("{id}.{attempt}.usage"))
}

/// The usage file next to a given `status_file` path (same id and attempt).
fn usage_file_for(status_file: &Path) -> PathBuf {
    status_file.with_extension("usage")
}

/// Token/cost usage an agent reported for one attempt, parsed from its
/// headless stdout. Only agents whose headless mode prints a final JSON
/// object with a `usage` field (e.g. Claude Code's `--output-format json`)
/// report anything; other agents simply have no usage file.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
struct Usage {
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_in: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tokens_out: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_usd: Option<f64>,
}

impl Usage {
    /// `None` if `bytes` isn't a JSON object reporting usage: either it
    /// isn't valid JSON (a non-JSON agent, or a headless run that failed
    /// before printing anything), or it has no `usage` to report.
    fn parse(bytes: &[u8]) -> Option<Usage> {
        let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
        let usage = value.get("usage")?;
        let tokens = |key: &str| usage.get(key).and_then(serde_json::Value::as_u64);
        let tokens_in = tokens("input_tokens").map(|n| {
            n + tokens("cache_creation_input_tokens").unwrap_or(0)
                + tokens("cache_read_input_tokens").unwrap_or(0)
        });
        let tokens_out = tokens("output_tokens");
        let cost_usd = value
            .get("total_cost_usd")
            .or_else(|| value.get("cost_usd"))
            .and_then(serde_json::Value::as_f64);
        (tokens_in.is_some() || tokens_out.is_some() || cost_usd.is_some()).then_some(Usage {
            tokens_in,
            tokens_out,
            cost_usd,
        })
    }

    fn to_json(self) -> String {
        serde_json::to_string(&self).expect("Usage always serializes")
    }
}

fn read_usage(path: &Path) -> Option<Usage> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn read_exit(path: &Path) -> Result<Option<Exit>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.parse().map_err(anyhow::Error::msg)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn tick(home: &Home, config: &Config) -> Result<()> {
    let _lock = home.lock()?;
    let now = now();
    enqueue_schedules(home, config, now)?;
    poll_running(home, config, now)?;
    start_pending(home, config, now)
}

// --- schedules ---------------------------------------------------------------

fn enqueue_schedules(home: &Home, config: &Config, now: DateTime<FixedOffset>) -> Result<()> {
    for path in md_files(&home.schedules())? {
        if let Err(e) = enqueue_schedule(home, config, &path, now) {
            log(&format!("schedule {}: {e:#}", path.display()));
        }
    }
    Ok(())
}

fn enqueue_schedule(
    home: &Home,
    config: &Config,
    path: &Path,
    now: DateTime<FixedOffset>,
) -> Result<()> {
    let mut s = Schedule::load(path)?;
    if !s.enabled {
        return Ok(());
    }
    let Some(last) = s.last_enqueued_at else {
        // First sighting: start counting from now instead of firing for a
        // slot that passed before the schedule existed.
        s.last_enqueued_at = Some(now);
        return s.save(path);
    };
    let slot = s
        .latest_at_or_before(now.with_timezone(&Local))?
        .fixed_offset();
    if slot <= last {
        return Ok(());
    }
    // Record the slot even if enqueueing fails, so a broken schedule logs
    // once per firing instead of once per poll.
    s.last_enqueued_at = Some(slot);
    s.save(path)?;

    let problems = s.problems(config, Some(path));
    if !problems.is_empty() {
        anyhow::bail!("not enqueued: {}", problems.join("; "));
    }
    let base = format!("{}-{}", truncate_id(&s.id, 48), slot.format("%Y%m%d-%H%M"));
    let mut task = Task::new(unique_id(home, &base), s.spec.clone(), s.prompt.clone());
    task.created_by = Some(format!("schedule:{}", s.id));
    task.create(&home.tasks().join(format!("{}.md", task.id)))?;
    log(&format!("enqueued {} from schedule {}", task.id, s.id));
    Ok(())
}

fn truncate_id(id: &str, max: usize) -> &str {
    id[..id.len().min(max)].trim_end_matches('-')
}

// --- running tasks -------------------------------------------------------------

enum Outcome {
    Succeeded(i32),
    Failed {
        error: String,
        exit_code: Option<i32>,
    },
    /// An interactive session ended without `lf done` / `lf fail`.
    NeedsReview,
}

fn poll_running(home: &Home, config: &Config, now: DateTime<FixedOffset>) -> Result<()> {
    let mut panes: HashMap<String, Vec<tmux::Pane>> = HashMap::new();
    for (path, mut task) in load_tasks(home, Status::Running)? {
        let (session, window) = window_of(&task, config);
        if !panes.contains_key(&session) {
            panes.insert(session.clone(), tmux::panes(&session)?);
        }
        let pane = panes[&session].iter().find(|p| p.window == window);
        let eff = task.spec.resolve(&config.defaults);
        let exit = read_exit(&status_file(home, &task.id, task.attempts))?;
        if exit.is_some()
            && let Some(usage) = read_usage(&usage_file(home, &task.id, task.attempts))
        {
            task.tokens_in = usage.tokens_in;
            task.tokens_out = usage.tokens_out;
            task.cost_usd = usage.cost_usd;
        }
        let outcome = match (exit, pane) {
            (Some(exit), _) => exit_outcome(eff.mode, exit),
            (None, None) => Outcome::Failed {
                error: "tmux window disappeared".into(),
                exit_code: None,
            },
            // `lf exec` writes the status before exiting, so a dead pane
            // without one means `lf exec` itself was killed.
            (None, Some(p)) if p.dead => Outcome::Failed {
                error: "the agent's window was killed".into(),
                exit_code: None,
            },
            (None, Some(_)) if eff.mode == Mode::Headless && timed_out(&task, &eff, now) => {
                Outcome::Failed {
                    error: format!(
                        "timed out after {}",
                        humantime::format_duration(eff.timeout)
                    ),
                    exit_code: None,
                }
            }
            (None, Some(_)) => continue,
        };
        if pane.is_some() {
            save_log(home, &task, &session, &window);
            tmux::kill_window(&session, &window)?;
        }
        if let Err(e) = finish(home, config, &path, task, outcome, now) {
            log(&format!("finishing {}: {e:#}", path.display()));
        }
    }
    Ok(())
}

fn exit_outcome(mode: Mode, exit: Exit) -> Outcome {
    match (mode, exit) {
        (Mode::Interactive, _) => Outcome::NeedsReview,
        (Mode::Headless, Exit::Code(0)) => Outcome::Succeeded(0),
        (Mode::Headless, Exit::Code(n)) => Outcome::Failed {
            error: format!("exited with status {n}"),
            exit_code: Some(n),
        },
        (Mode::Headless, Exit::Signal(s)) => Outcome::Failed {
            error: format!("killed by signal {s}"),
            exit_code: None,
        },
    }
}

fn timed_out(task: &Task, eff: &Effective, now: DateTime<FixedOffset>) -> bool {
    task.started_at
        .and_then(|s| (now - s).to_std().ok())
        .is_some_and(|elapsed| elapsed > eff.timeout)
}

/// `(session, window)` the task runs in: what was recorded at start, or the
/// configured session and the task id.
pub fn window_of(task: &Task, config: &Config) -> (String, String) {
    match task.tmux_window.as_deref().and_then(|w| w.split_once(':')) {
        Some((s, w)) => (s.to_string(), w.to_string()),
        None => (config.runner.tmux_session.clone(), task.id.clone()),
    }
}

pub fn save_log(home: &Home, task: &Task, session: &str, window: &str) {
    let path = home
        .logs()
        .join(format!("{}.{}.log", task.id, task.attempts));
    let result = tmux::capture(session, window)
        .and_then(|text| std::fs::write(&path, text).map_err(Into::into));
    if let Err(e) = result {
        log(&format!("{}: could not save the log: {e:#}", task.id));
    }
}

fn finish(
    home: &Home,
    config: &Config,
    path: &Path,
    mut task: Task,
    outcome: Outcome,
    now: DateTime<FixedOffset>,
) -> Result<()> {
    let eff = task.spec.resolve(&config.defaults);
    match outcome {
        Outcome::Succeeded(code) => {
            task.exit_code = Some(code);
            complete(home, config, path, task, now).map(drop)
        }
        Outcome::NeedsReview => {
            task.status = Status::NeedsReview;
            archive(home, path, task, now)
        }
        Outcome::Failed { error, exit_code } => {
            task.exit_code = exit_code;
            fail_attempt(home, path, task, &eff, error, now)
        }
    }
}

/// Runs `on_finish` and archives the task as done, or as failed if
/// `on_finish` fails. Returns the final status.
pub fn complete(
    home: &Home,
    config: &Config,
    path: &Path,
    mut task: Task,
    now: DateTime<FixedOffset>,
) -> Result<Status> {
    let eff = task.spec.resolve(&config.defaults);
    let dir = workdir(home, &task, &eff);
    match git::on_finish(eff.on_finish, &dir, &task.id, &task.prompt) {
        Ok(()) => {
            task.status = Status::Done;
            task.error = None;
        }
        Err(e) => {
            task.status = Status::Failed;
            task.error = Some(format!("on_finish failed: {e:#}"));
        }
    }
    let status = task.status;
    archive(home, path, task, now)?;
    Ok(status)
}

/// Requeues the task after `retry_delay` if it has retries left, otherwise
/// archives it as failed.
fn fail_attempt(
    home: &Home,
    path: &Path,
    mut task: Task,
    eff: &Effective,
    error: String,
    now: DateTime<FixedOffset>,
) -> Result<()> {
    task.error = Some(error);
    task.tmux_window = None;
    if retry_left(task.attempts, eff.retries) {
        task.status = Status::Pending;
        task.scheduled_at = Some(now + eff.retry_delay);
        log(&format!(
            "{} failed (attempt {}): {}; retrying in {}",
            task.id,
            task.attempts,
            task.error.as_deref().unwrap_or_default(),
            humantime::format_duration(eff.retry_delay)
        ));
        task.save(path)
    } else {
        task.status = Status::Failed;
        archive(home, path, task, now)
    }
}

/// `retries` counts extra attempts after the first.
fn retry_left(attempts: u32, retries: u32) -> bool {
    attempts <= retries
}

/// Stamps `finished_at` and moves the task to `archive/` (a no-op move if
/// it's already there).
pub fn archive(home: &Home, path: &Path, mut task: Task, now: DateTime<FixedOffset>) -> Result<()> {
    task.finished_at = Some(now);
    task.save(path)?;
    let dest = home.archive().join(format!("{}.md", task.id));
    std::fs::rename(path, &dest)
        .with_context(|| format!("moving {} to {}", path.display(), dest.display()))?;
    let detail = task
        .error
        .as_deref()
        .map(|e| format!(": {e}"))
        .unwrap_or_default();
    log(&format!("{} {}{detail}", task.id, task.status.as_str()));
    Ok(())
}

// --- starting tasks ------------------------------------------------------------

fn start_pending(home: &Home, config: &Config, now: DateTime<FixedOffset>) -> Result<()> {
    let mut per_repo: HashMap<PathBuf, usize> = HashMap::new();
    let running = load_tasks(home, Status::Running)?;
    for (_, t) in &running {
        *per_repo
            .entry(t.spec.resolve(&config.defaults).repo)
            .or_default() += 1;
    }
    let mut total = running.len();

    let mut pending: Vec<_> = load_tasks(home, Status::Pending)?
        .into_iter()
        .filter(|(_, t)| t.scheduled_at.is_none_or(|at| at <= now))
        .collect();
    pending.sort_by_key(|(_, t)| (t.scheduled_at.or(t.created_at), t.id.clone()));

    for (path, task) in pending {
        if total >= config.runner.max_parallel {
            break;
        }
        let repo = task.spec.resolve(&config.defaults).repo;
        let in_repo = per_repo.entry(repo).or_default();
        if *in_repo >= config.runner.max_parallel_per_repo {
            continue;
        }
        match start(home, config, &path, task, now) {
            Ok(true) => {
                total += 1;
                *in_repo += 1;
            }
            Ok(false) => {}
            Err(e) => log(&format!("starting {}: {e:#}", path.display())),
        }
    }
    Ok(())
}

/// Starts one attempt. Returns whether the task is now running.
fn start(
    home: &Home,
    config: &Config,
    path: &Path,
    mut task: Task,
    now: DateTime<FixedOffset>,
) -> Result<bool> {
    let eff = task.spec.resolve(&config.defaults);
    let problems = task.problems(config, Some(path));
    if !problems.is_empty() {
        task.status = Status::Failed;
        task.error = Some(format!("invalid task: {}", problems.join("; ")));
        archive(home, path, task, now)?;
        return Ok(false);
    }

    task.attempts += 1;
    task.started_at = Some(now);
    let session = &config.runner.tmux_session;
    let launched = prepare_workdir(home, config, &task, &eff).and_then(|dir| {
        // A leftover window with the same name would make targets ambiguous.
        tmux::kill_window(session, &task.id)?;
        // A record left by an earlier task with this id would end this
        // attempt as soon as it's polled.
        let status = status_file(home, &task.id, task.attempts);
        remove_if_exists(&status)?;
        tmux::spawn(
            session,
            &task.id,
            &dir,
            &exec_argv(home, &task.id, &status)?,
        )
    });
    match launched {
        Ok(()) => {
            task.status = Status::Running;
            task.tmux_window = Some(format!("{session}:{}", task.id));
            task.save(path)?;
            log(&format!(
                "started {} (attempt {}) in tmux {session}:{}",
                task.id, task.attempts, task.id
            ));
            Ok(true)
        }
        Err(e) => {
            fail_attempt(
                home,
                path,
                task,
                &eff,
                format!("could not start: {e:#}"),
                now,
            )?;
            Ok(false)
        }
    }
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing {}", path.display()))
        }
        _ => Ok(()),
    }
}

pub fn workdir(home: &Home, task: &Task, eff: &Effective) -> PathBuf {
    if eff.worktree {
        home.worktrees().join(&task.id)
    } else {
        eff.repo.clone()
    }
}

fn prepare_workdir(home: &Home, config: &Config, task: &Task, eff: &Effective) -> Result<PathBuf> {
    let dir = workdir(home, task, eff);
    match task.effective_branch(config) {
        Some(branch) if eff.worktree => git::ensure_worktree(&eff.repo, &dir, &branch)?,
        Some(branch) => git::checkout_branch(&eff.repo, &branch)?,
        None => {}
    }
    Ok(dir)
}

fn exec_argv(home: &Home, id: &str, status_file: &Path) -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("locating the lf binary")?;
    let path = |p: &Path| {
        p.to_str()
            .map(String::from)
            .context("path is not valid UTF-8")
    };
    Ok(vec![
        path(&exe)?,
        "--home".into(),
        path(home.root())?,
        "exec".into(),
        id.into(),
        "--status-file".into(),
        path(status_file)?,
    ])
}

/// The agent's argv for `mode`, with `model_args` appended if there's a
/// model. `{prompt}` is filled last so placeholders inside the prompt text
/// stay literal.
fn agent_argv(
    agent: &Agent,
    mode: Mode,
    prompt: &str,
    model: Option<&str>,
    id: &str,
) -> Vec<String> {
    let template = match mode {
        Mode::Headless => &agent.headless,
        Mode::Interactive => &agent.interactive,
    };
    let model_args = match model {
        Some(_) => agent.model_args.as_slice(),
        None => &[],
    };
    template
        .iter()
        .chain(model_args)
        .map(|a| {
            a.replace("{model}", model.unwrap_or_default())
                .replace("{id}", id)
                .replace("{prompt}", prompt)
        })
        .collect()
}

// --- helpers -------------------------------------------------------------------

fn load_tasks(home: &Home, status: Status) -> Result<Vec<(PathBuf, Task)>> {
    let mut out = Vec::new();
    for path in md_files(&home.tasks())? {
        match Task::load(&path) {
            Ok(t) if t.status == status => out.push((path, t)),
            Ok(_) => {}
            Err(e) => log(&format!("skipping {}: {e:#}", path.display())),
        }
    }
    Ok(out)
}

pub fn log(msg: &str) {
    println!("{} {msg}", Local::now().format("%Y-%m-%d %H:%M:%S"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_agent_placeholders() {
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let agent = Agent {
            headless: argv(&["agent", "run", "{prompt}", "x-{id}"]),
            interactive: argv(&["agent", "{prompt}"]),
            model_args: argv(&["-m", "{model}"]),
            model: None,
        };
        let prompt = "fix {model}; rm -rf";
        assert_eq!(
            agent_argv(&agent, Mode::Headless, prompt, Some("big"), "t1"),
            argv(&["agent", "run", prompt, "x-t1", "-m", "big"])
        );
        assert_eq!(
            agent_argv(&agent, Mode::Interactive, prompt, None, "t1"),
            argv(&["agent", prompt])
        );
    }

    #[test]
    fn retries_count_extra_attempts() {
        assert!(retry_left(1, 1));
        assert!(!retry_left(2, 1));
        assert!(!retry_left(1, 0));
    }

    #[test]
    fn maps_exit_status_to_outcome() {
        use Exit::*;
        assert!(matches!(
            exit_outcome(Mode::Headless, Code(0)),
            Outcome::Succeeded(0)
        ));
        assert!(matches!(
            exit_outcome(Mode::Headless, Code(2)),
            Outcome::Failed {
                exit_code: Some(2),
                ..
            }
        ));
        assert!(matches!(
            exit_outcome(Mode::Headless, Signal(9)),
            Outcome::Failed { .. }
        ));
        assert!(matches!(
            exit_outcome(Mode::Interactive, Code(0)),
            Outcome::NeedsReview
        ));
    }

    #[test]
    fn exit_records_roundtrip() {
        for e in [Exit::Code(0), Exit::Code(127), Exit::Signal(15)] {
            assert_eq!(e.to_string().parse::<Exit>(), Ok(e));
        }
        assert!("garbage".parse::<Exit>().is_err());
    }

    #[test]
    fn truncates_ids_without_trailing_dash() {
        assert_eq!(truncate_id("nightly-deps", 48), "nightly-deps");
        assert_eq!(truncate_id("abc-def", 4), "abc");
    }

    #[test]
    fn parses_claude_code_usage() {
        let stdout = r#"{"type":"result","total_cost_usd":0.0512,"usage":{"input_tokens":120,"cache_creation_input_tokens":30,"cache_read_input_tokens":500,"output_tokens":80}}"#;
        let usage = Usage::parse(stdout.as_bytes()).unwrap();
        assert_eq!(usage.tokens_in, Some(120 + 30 + 500));
        assert_eq!(usage.tokens_out, Some(80));
        assert_eq!(usage.cost_usd, Some(0.0512));
    }

    #[test]
    fn no_usage_for_non_json_or_usage_less_output() {
        assert!(Usage::parse(b"plain text output, not json").is_none());
        assert!(Usage::parse(br#"{"type":"result","result":"ok"}"#).is_none());
    }

    #[test]
    fn usage_roundtrips_through_json() {
        let usage = Usage {
            tokens_in: Some(650),
            tokens_out: Some(80),
            cost_usd: Some(0.0512),
        };
        let read_back: Usage = serde_json::from_str(&usage.to_json()).unwrap();
        assert_eq!(usage, read_back);
    }
}
