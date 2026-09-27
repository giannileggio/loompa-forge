//! Changing a task's status by hand: `lf done|fail|cancel|retry|attach`.
//!
//! These hold the home lock while they touch task files, so they never race
//! a `tick` of `lf run`. The id defaults to `$LF_TASK_ID`, which `lf run`
//! sets in each task's window, so `lf done` works from inside a session.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::home::Home;
use crate::run;
use crate::task::{Status, Task, now};
use crate::tmux;

/// Runs `on_finish` and archives the task as done. Works on running tasks
/// (ending their session), on `needs_review` ones, and on failed ones, e.g.
/// after fixing whatever made `on_finish` fail.
pub fn done(home: &Home, id: Option<String>) -> Result<()> {
    let (config, id) = setup(home, id)?;
    let lock = home.lock()?;
    let (path, task) = find(home, &id)?;
    let deferred = match task.status {
        Status::Running => end_session(home, &config, &task)?,
        Status::NeedsReview | Status::Failed => None,
        s => bail!(
            "`{id}` is {}; only running, needs_review or failed tasks can be marked done",
            s.as_str()
        ),
    };
    let status = run::complete(home, &config, &path, task, now())?;
    drop(lock);
    kill_deferred(deferred)?;
    if status != Status::Done {
        bail!("on_finish failed; the work is still in the task's worktree or repo");
    }
    Ok(())
}

/// Archives the task as failed, without retrying.
pub fn fail(home: &Home, id: Option<String>, reason: Option<String>) -> Result<()> {
    let (config, id) = setup(home, id)?;
    let lock = home.lock()?;
    let (path, mut task) = find(home, &id)?;
    let deferred = match task.status {
        Status::Running => end_session(home, &config, &task)?,
        Status::NeedsReview => None,
        s => bail!(
            "`{id}` is {}; only running or needs_review tasks can be marked failed",
            s.as_str()
        ),
    };
    task.status = Status::Failed;
    task.error = Some(reason.unwrap_or_else(|| "marked failed with `lf fail`".into()));
    run::archive(home, &path, task, now())?;
    drop(lock);
    kill_deferred(deferred)
}

/// Archives a pending or running task as cancelled. `on_finish` doesn't run.
pub fn cancel(home: &Home, id: Option<String>) -> Result<()> {
    let (config, id) = setup(home, id)?;
    let lock = home.lock()?;
    let (path, mut task) = find(home, &id)?;
    let deferred = match task.status {
        Status::Running => end_session(home, &config, &task)?,
        Status::Pending => None,
        s => bail!(
            "`{id}` is {}; only pending or running tasks can be cancelled",
            s.as_str()
        ),
    };
    task.status = Status::Cancelled;
    run::archive(home, &path, task, now())?;
    drop(lock);
    kill_deferred(deferred)
}

/// Moves a failed, cancelled or `needs_review` task back to the queue, to
/// start as soon as possible in the same worktree. `attempts` keeps
/// counting, so logs aren't overwritten.
pub fn retry(home: &Home, id: &str) -> Result<()> {
    home.ensure_initialized()?;
    let _lock = home.lock()?;
    let (path, mut task) = find(home, id)?;
    if !matches!(
        task.status,
        Status::Failed | Status::Cancelled | Status::NeedsReview
    ) {
        bail!(
            "`{id}` is {}; only failed, cancelled or needs_review tasks can be retried",
            task.status.as_str()
        );
    }
    task.status = Status::Pending;
    task.scheduled_at = None;
    task.started_at = None;
    task.finished_at = None;
    task.exit_code = None;
    task.tmux_window = None;
    task.error = None;
    let dest = home.tasks().join(format!("{id}.md"));
    task.create(&dest)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    run::log(&format!("{id} requeued"));
    Ok(())
}

/// Attaches to a running task's window, or to the whole session.
pub fn attach(home: &Home, id: Option<String>) -> Result<()> {
    home.ensure_initialized()?;
    let config = Config::load(&home.config_path())?;
    let Some(id) = id else {
        let session = &config.runner.tmux_session;
        if !tmux::session_exists(session) {
            bail!("no tmux session `{session}`: nothing is running");
        }
        return Err(tmux::attach(session, None));
    };
    let (_, task) = find(home, &id)?;
    if task.status != Status::Running {
        bail!("`{id}` is {}, not running", task.status.as_str());
    }
    let (session, window) = run::window_of(&task, &config);
    Err(tmux::attach(&session, Some(&window)))
}

/// A task's captured output: the live tmux pane if it's still running,
/// else the saved log for an attempt (the latest, by default).
pub fn logs(home: &Home, id: &str, attempt: Option<u32>) -> Result<String> {
    home.ensure_initialized()?;
    let config = Config::load(&home.config_path())?;
    let (_, task) = find(home, id)?;
    if task.attempts == 0 {
        bail!("`{id}` hasn't started yet");
    }
    let attempt = attempt.unwrap_or(task.attempts);
    if attempt == 0 || attempt > task.attempts {
        bail!("`{id}` has {} attempt(s) so far", task.attempts);
    }
    if task.status == Status::Running && attempt == task.attempts {
        let (session, window) = run::window_of(&task, &config);
        if tmux::session_exists(&session) {
            return tmux::capture(&session, &window);
        }
    }
    let path = home.logs().join(format!("{id}.{attempt}.log"));
    std::fs::read_to_string(&path)
        .with_context(|| format!("no log saved for `{id}` attempt {attempt}"))
}

fn setup(home: &Home, id: Option<String>) -> Result<(Config, String)> {
    home.ensure_initialized()?;
    let config = Config::load(&home.config_path())?;
    let id = id
        .or_else(|| std::env::var("LF_TASK_ID").ok())
        .context("no task id: pass one, or run this inside a task's window")?;
    Ok((config, id))
}

/// The task's file, in `tasks/` or else `archive/`.
fn find(home: &Home, id: &str) -> Result<(PathBuf, Task)> {
    for dir in [home.tasks(), home.archive()] {
        let path = dir.join(format!("{id}.md"));
        if path.exists() {
            let task = Task::load(&path)?;
            return Ok((path, task));
        }
    }
    bail!("no task `{id}` in tasks/ or archive/")
}

type Window = (String, String);

/// Saves a running task's output and kills its window, so the agent stops
/// before `on_finish` touches its work. From inside that window, killing it
/// would kill this command too, so the kill is returned for after the task
/// is archived.
fn end_session(home: &Home, config: &Config, task: &Task) -> Result<Option<Window>> {
    let (session, window) = run::window_of(task, config);
    run::save_log(home, task, &session, &window);
    if std::env::var("LF_TASK_ID").is_ok_and(|v| v == task.id) {
        return Ok(Some((session, window)));
    }
    tmux::kill_window(&session, &window)?;
    Ok(None)
}

fn kill_deferred(window: Option<Window>) -> Result<()> {
    match window {
        Some((session, window)) => tmux::kill_window(&session, &window),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::TaskSpec;

    fn home() -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::resolve(Some(dir.path().to_path_buf())).unwrap();
        for d in home.dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        (dir, home)
    }

    fn put(dir: PathBuf, id: &str, status: Status) {
        let spec = TaskSpec {
            repo: std::env::temp_dir(),
            branch: None,
            worktree: Some(false),
            agent: None,
            model: None,
            mode: None,
            on_finish: None,
            retries: None,
            retry_delay: None,
            timeout: None,
        };
        let mut task = Task::new(id.into(), spec, "do it".into());
        task.status = status;
        task.error = Some("earlier failure".into());
        task.save(&dir.join(format!("{id}.md"))).unwrap();
    }

    fn status_of(home: &Home, id: &str) -> (Status, &'static str) {
        let (path, task) = find(home, id).unwrap();
        let place = if path.starts_with(home.archive()) {
            "archive"
        } else {
            "tasks"
        };
        (task.status, place)
    }

    #[test]
    fn cancel_then_retry_round_trip() {
        let (_d, home) = home();
        put(home.tasks(), "t1", Status::Pending);

        cancel(&home, Some("t1".into())).unwrap();
        assert_eq!(status_of(&home, "t1"), (Status::Cancelled, "archive"));
        assert!(cancel(&home, Some("t1".into())).is_err());

        retry(&home, "t1").unwrap();
        assert_eq!(status_of(&home, "t1"), (Status::Pending, "tasks"));
        let (_, t) = find(&home, "t1").unwrap();
        assert_eq!((t.finished_at, t.error), (None, None));
        assert!(!home.archive().join("t1.md").exists());
    }

    #[test]
    fn review_outcomes() {
        let (_d, home) = home();
        put(home.archive(), "ok", Status::NeedsReview);
        put(home.archive(), "bad", Status::NeedsReview);

        done(&home, Some("ok".into())).unwrap();
        let (_, t) = find(&home, "ok").unwrap();
        assert_eq!((t.status, t.error), (Status::Done, None));

        fail(&home, Some("bad".into()), Some("wrong approach".into())).unwrap();
        let (_, t) = find(&home, "bad").unwrap();
        assert_eq!(t.status, Status::Failed);
        assert_eq!(t.error.as_deref(), Some("wrong approach"));
    }

    #[test]
    fn logs_reports_missing_attempts_and_reads_saved_ones() {
        let (_d, home) = home();
        put(home.tasks(), "p", Status::Pending);
        assert!(logs(&home, "p", None).is_err());

        let (path, mut task) = find(&home, "p").unwrap();
        task.attempts = 2;
        task.status = Status::Failed;
        task.save(&path).unwrap();
        std::fs::write(home.logs().join("p.1.log"), "first\n").unwrap();
        std::fs::write(home.logs().join("p.2.log"), "second\n").unwrap();

        assert_eq!(logs(&home, "p", None).unwrap(), "second\n");
        assert_eq!(logs(&home, "p", Some(1)).unwrap(), "first\n");
        assert!(logs(&home, "p", Some(3)).is_err());
        assert!(logs(&home, "p", Some(0)).is_err());
    }

    #[test]
    fn rejects_wrong_status_and_unknown_ids() {
        let (_d, home) = home();
        put(home.tasks(), "p", Status::Pending);
        assert!(done(&home, Some("p".into())).is_err());
        assert!(fail(&home, Some("p".into()), None).is_err());
        assert!(retry(&home, "p").is_err());
        assert!(done(&home, Some("nope".into())).is_err());
        assert_eq!(status_of(&home, "p"), (Status::Pending, "tasks"));
    }
}
