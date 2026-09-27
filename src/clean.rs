//! `lf clean`: remove the worktrees of finished tasks.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};

use crate::git;
use crate::home::Home;
use crate::task::{Status, Task, now};

#[derive(clap::Args)]
pub struct CleanArgs {
    /// Also clean failed, cancelled and needs_review tasks, and worktrees
    /// with no task file.
    #[arg(long)]
    all: bool,
    /// Only tasks that finished at least this long ago, e.g. 7d.
    #[arg(long, value_parser = humantime::parse_duration)]
    older_than: Option<Duration>,
    /// Show what would be removed without removing anything.
    #[arg(long, short = 'n')]
    dry_run: bool,
}

/// Removes `worktrees/<id>` for archived tasks that qualify. Worktrees with
/// uncommitted changes are always kept; branches are never deleted.
pub fn clean(home: &Home, args: CleanArgs) -> Result<()> {
    home.ensure_initialized()?;
    // Keeps `lf run` from starting (or `lf retry` from requeueing) a task
    // whose worktree is being removed.
    let _lock = home.lock()?;
    let now = now();
    let mut dirs: Vec<_> = std::fs::read_dir(home.worktrees())
        .with_context(|| format!("reading {}", home.worktrees().display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let (mut removed, mut kept) = (0, 0);
    for dir in dirs {
        let id = dir.file_name().unwrap_or_default().to_string_lossy();
        let verdict = match task_state(home, &id)? {
            State::Queued => Err("still queued or running".to_string()),
            State::Archived(status, finished_at) => {
                eligible(status, finished_at, args.all, args.older_than, now)
            }
            State::Orphan if args.older_than.is_some() => {
                Err("no task file, so its age is unknown".into())
            }
            State::Orphan if args.all => Ok(()),
            State::Orphan => Err("no task file (use --all)".into()),
        }
        .and_then(|()| check_clean(&dir));
        match verdict {
            Err(why) => {
                kept += 1;
                println!("kept     {id}: {why}");
            }
            Ok(()) if args.dry_run => {
                removed += 1;
                println!("would remove {id}");
            }
            Ok(()) => match git::remove_worktree(&dir) {
                Ok(()) => {
                    removed += 1;
                    println!("removed  {id}");
                }
                Err(e) => {
                    kept += 1;
                    println!("kept     {id}: {e:#}");
                }
            },
        }
    }
    let verb = if args.dry_run {
        "would remove"
    } else {
        "removed"
    };
    println!(
        "{verb} {removed}, kept {kept} (in {})",
        home.worktrees().display()
    );
    Ok(())
}

enum State {
    Queued,
    Archived(Status, Option<DateTime<FixedOffset>>),
    Orphan,
}

fn task_state(home: &Home, id: &str) -> Result<State> {
    let file = format!("{id}.md");
    if home.tasks().join(&file).exists() {
        return Ok(State::Queued);
    }
    let archived = home.archive().join(&file);
    if !archived.exists() {
        return Ok(State::Orphan);
    }
    let task = Task::load(&archived)?;
    Ok(State::Archived(task.status, task.finished_at))
}

/// Whether an archived task's worktree may go, or why not.
fn eligible(
    status: Status,
    finished_at: Option<DateTime<FixedOffset>>,
    all: bool,
    older_than: Option<Duration>,
    now: DateTime<FixedOffset>,
) -> Result<(), String> {
    if status != Status::Done && !all {
        return Err(format!("{} (use --all)", status.as_str()));
    }
    if let Some(min) = older_than {
        let age = finished_at.and_then(|f| (now - f).to_std().ok());
        if age.is_none_or(|a| a < min) {
            return Err("finished too recently".into());
        }
    }
    Ok(())
}

fn check_clean(dir: &Path) -> Result<(), String> {
    match git::has_changes(dir) {
        Ok(false) => Ok(()),
        Ok(true) => Err("has uncommitted changes".into()),
        Err(e) => Err(format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligibility() {
        let now = now();
        let day = Duration::from_secs(86400);
        let ago = |d: u32| Some(now - chrono::Duration::days(d.into()));

        assert!(eligible(Status::Done, ago(0), false, None, now).is_ok());
        assert!(eligible(Status::Failed, ago(0), false, None, now).is_err());
        assert!(eligible(Status::Failed, ago(0), true, None, now).is_ok());

        assert!(eligible(Status::Done, ago(8), false, Some(7 * day), now).is_ok());
        assert!(eligible(Status::Done, ago(2), false, Some(7 * day), now).is_err());
        assert!(eligible(Status::Done, None, false, Some(day), now).is_err());
    }
}
