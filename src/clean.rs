//! `lf clean`: remove the worktrees of finished tasks, and optionally the
//! archived task files and logs themselves.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset};

use crate::cmd::md_files;
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
    /// Also delete the archived task file and its logs, for tasks whose
    /// worktree is already gone (or never had one). Loses that task's
    /// history from `lf ls --archive` / `lf web` for good.
    #[arg(long)]
    archive: bool,
    /// Show what would be removed without removing anything.
    #[arg(long, short = 'n')]
    dry_run: bool,
}

/// Removes `worktrees/<id>` for archived tasks that qualify, and with
/// `--archive`, also the archived task file and its logs. Worktrees with
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
            State::Unreadable => Err("unreadable task file".to_string()),
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

    if args.archive {
        prune_archive(home, &args, now)?;
    }
    Ok(())
}

/// Deletes archived task files (and their logs) that qualify, but only if
/// their worktree is already gone — a worktree left behind because it had
/// uncommitted changes keeps its task record too, so `lf clean --all` run
/// again still explains what that worktree is.
fn prune_archive(home: &Home, args: &CleanArgs, now: DateTime<FixedOffset>) -> Result<()> {
    let (mut removed, mut kept) = (0, 0);
    for path in md_files(&home.archive())? {
        let id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        // A half-written or hand-edited file is reported and
        // kept, like the runner does, instead of aborting the
        // whole clean.
        let task = match Task::load(&path) {
            Ok(task) => task,
            Err(e) => {
                kept += 1;
                println!("kept     {id}: unreadable task file: {e:#}");
                continue;
            }
        };
        let id = task.id.clone();
        let verdict = eligible(
            task.status,
            task.finished_at,
            args.all,
            args.older_than,
            now,
        )
        .and_then(|()| {
            if home.worktrees().join(&id).exists() {
                Err("worktree still present".to_string())
            } else {
                Ok(())
            }
        });
        match verdict {
            Err(why) => {
                kept += 1;
                println!("kept     {id}: {why}");
            }
            Ok(()) if args.dry_run => {
                removed += 1;
                println!("would remove {id} (archive + logs)");
            }
            Ok(()) => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
                remove_logs(home, &id)?;
                removed += 1;
                println!("removed  {id} (archive + logs)");
            }
        }
    }
    let verb = if args.dry_run {
        "would remove"
    } else {
        "removed"
    };
    println!(
        "{verb} {removed}, kept {kept} (in {})",
        home.archive().display()
    );
    Ok(())
}

/// Removes every `logs/<id>.*` file (all attempts: `.log`, `.exit`, `.usage`).
fn remove_logs(home: &Home, id: &str) -> Result<()> {
    let prefix = format!("{id}.");
    let dir = home.logs();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("reading {}", dir.display()))?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            std::fs::remove_file(entry.path())
                .with_context(|| format!("removing {}", entry.path().display()))?;
        }
    }
    Ok(())
}

enum State {
    Queued,
    Archived(Status, Option<DateTime<FixedOffset>>),
    /// The archived file is there but can't be read (half-written
    /// or hand-edited). Kept as-is, worktree included.
    Unreadable,
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
    match Task::load(&archived) {
        Ok(task) => Ok(State::Archived(task.status, task.finished_at)),
        Err(e) => {
            println!("warning: skipping {}: {e:#}", archived.display());
            Ok(State::Unreadable)
        }
    }
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
    use crate::home::Home;
    use crate::spec::TaskSpec;

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

    fn home() -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::resolve(Some(dir.path().to_path_buf())).unwrap();
        for d in home.dirs() {
            std::fs::create_dir_all(d).unwrap();
        }
        (dir, home)
    }

    fn archive(home: &Home, id: &str, status: Status, finished_at: Option<DateTime<FixedOffset>>) {
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
        task.finished_at = finished_at;
        task.save(&home.archive().join(format!("{id}.md"))).unwrap();
        for attempt in [1, 2] {
            std::fs::write(home.logs().join(format!("{id}.{attempt}.log")), "output\n").unwrap();
        }
    }

    #[test]
    fn archive_flag_prunes_eligible_task_files_and_logs() {
        let (_d, home) = home();
        archive(&home, "old-done", Status::Done, Some(now()));
        archive(&home, "old-failed", Status::Failed, Some(now()));

        let args = CleanArgs {
            all: false,
            older_than: None,
            archive: true,
            dry_run: false,
        };
        clean(&home, args).unwrap();

        assert!(!home.archive().join("old-done.md").exists());
        assert!(!home.logs().join("old-done.1.log").exists());
        assert!(!home.logs().join("old-done.2.log").exists());
        // Failed, without --all: kept.
        assert!(home.archive().join("old-failed.md").exists());
        assert!(home.logs().join("old-failed.1.log").exists());
    }

    #[test]
    fn archive_flag_keeps_tasks_whose_worktree_is_still_present() {
        let (_d, home) = home();
        archive(&home, "has-worktree", Status::Done, Some(now()));
        std::fs::create_dir_all(home.worktrees().join("has-worktree")).unwrap();

        let args = CleanArgs {
            all: false,
            older_than: None,
            archive: true,
            dry_run: false,
        };
        clean(&home, args).unwrap();

        assert!(home.archive().join("has-worktree.md").exists());
    }

    #[test]
    fn dry_run_does_not_delete_archive_files() {
        let (_d, home) = home();
        archive(&home, "old-done", Status::Done, Some(now()));

        let args = CleanArgs {
            all: false,
            older_than: None,
            archive: true,
            dry_run: true,
        };
        clean(&home, args).unwrap();

        assert!(home.archive().join("old-done.md").exists());
        assert!(home.logs().join("old-done.1.log").exists());
    }

    #[test]
    fn without_archive_flag_task_files_are_untouched() {
        let (_d, home) = home();
        archive(&home, "old-done", Status::Done, Some(now()));

        let args = CleanArgs {
            all: false,
            older_than: None,
            archive: false,
            dry_run: false,
        };
        clean(&home, args).unwrap();

        assert!(home.archive().join("old-done.md").exists());
    }

    /// A half-written or hand-edited archived task file is
    /// reported and kept (with its worktree), not fatal and not
    /// deleted.
    #[test]
    fn clean_skips_and_reports_unreadable_task_files() {
        let (_d, home) = home();
        archive(&home, "good", Status::Done, Some(now()));
        std::fs::write(
            home.archive().join("bad.md"),
            "---\nid: bad\nstatus: ", // frontmatter never closed
        )
        .unwrap();
        std::fs::create_dir_all(home.worktrees().join("bad")).unwrap();

        let args = CleanArgs {
            all: true,
            older_than: None,
            archive: true,
            dry_run: false,
        };
        clean(&home, args).unwrap();

        assert!(!home.archive().join("good.md").exists());
        assert!(
            home.archive().join("bad.md").exists(),
            "an unreadable file is kept, not dropped"
        );
        assert!(
            home.worktrees().join("bad").exists(),
            "its worktree is kept too"
        );
    }
}
