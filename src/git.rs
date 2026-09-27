//! Git and `gh` operations the runner needs, run as argv (no shell).

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::spec::OnFinish;

fn run(program: &str, dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .with_context(|| format!("running {program} (is it installed?)"))?;
    if !out.status.success() {
        bail!(
            "`{program} {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run("git", dir, args)
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok()
}

/// Makes `path` a worktree of `repo` on `branch`, creating the branch from
/// the repo's HEAD if needed. An existing `path` is reused as is, so a
/// retried task picks up where the previous attempt left off.
pub fn ensure_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    let p = path.to_str().context("worktree path is not valid UTF-8")?;
    if branch_exists(repo, branch) {
        git(repo, &["worktree", "add", p, branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", branch, p])?;
    }
    Ok(())
}

/// Checks out `branch` in `repo` itself, creating it if needed.
pub fn checkout_branch(repo: &Path, branch: &str) -> Result<()> {
    if branch_exists(repo, branch) {
        git(repo, &["checkout", branch])?;
    } else {
        git(repo, &["checkout", "-b", branch])?;
    }
    Ok(())
}

pub fn has_changes(dir: &Path) -> Result<bool> {
    Ok(!git(dir, &["status", "--porcelain"])?.trim().is_empty())
}

/// Commits everything in `dir`. Returns false if there was nothing to commit.
pub fn commit_all(dir: &Path, message: &str) -> Result<bool> {
    if !has_changes(dir)? {
        return Ok(false);
    }
    git(dir, &["add", "-A"])?;
    git(dir, &["commit", "-q", "-m", message])?;
    Ok(true)
}

/// Commit message for a task: the prompt's first line as the subject (cut
/// to 72 chars), the whole prompt as the body.
pub fn commit_message(id: &str, prompt: &str) -> String {
    let first = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(id)
        .trim();
    let subject: String = if first.chars().count() > 72 {
        first.chars().take(71).chain(['…']).collect()
    } else {
        first.to_string()
    };
    if prompt.trim() == subject {
        subject
    } else {
        format!("{subject}\n\n{}\n\nlf task: {id}", prompt.trim())
    }
}

/// Runs the task's `on_finish` step in `dir`.
pub fn on_finish(action: OnFinish, dir: &Path, id: &str, prompt: &str) -> Result<()> {
    if action == OnFinish::None {
        return Ok(());
    }
    let message = commit_message(id, prompt);
    commit_all(dir, &message)?;
    if action == OnFinish::Commit {
        return Ok(());
    }
    git(dir, &["push", "-u", "origin", "HEAD"])?;
    if action == OnFinish::Pr {
        let (title, body) = message.split_once("\n\n").unwrap_or((&message, ""));
        run(
            "gh",
            dir,
            &["pr", "create", "--title", title, "--body", body],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "Test"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git(p, args).unwrap();
        }
        dir
    }

    fn current_branch(dir: &Path) -> String {
        git(dir, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .into()
    }

    #[test]
    fn creates_and_reuses_worktrees() {
        let repo = repo();
        let wt = repo.path().join("../").join(format!(
            "{}-wt",
            repo.path().file_name().unwrap().to_str().unwrap()
        ));
        ensure_worktree(repo.path(), &wt, "lf/new").unwrap();
        assert_eq!(current_branch(&wt), "lf/new");

        std::fs::write(wt.join("f.txt"), "x").unwrap();
        ensure_worktree(repo.path(), &wt, "lf/new").unwrap();
        assert!(wt.join("f.txt").exists(), "existing worktree is reused");

        // An existing branch is checked out rather than recreated.
        git(repo.path(), &["branch", "existing"]).unwrap();
        let wt2 = wt.with_extension("2");
        ensure_worktree(repo.path(), &wt2, "existing").unwrap();
        assert_eq!(current_branch(&wt2), "existing");

        std::fs::remove_dir_all(&wt).unwrap();
        std::fs::remove_dir_all(&wt2).unwrap();
    }

    #[test]
    fn checks_out_branches_in_place() {
        let repo = repo();
        checkout_branch(repo.path(), "feature").unwrap();
        assert_eq!(current_branch(repo.path()), "feature");
        checkout_branch(repo.path(), "main").unwrap();
        assert_eq!(current_branch(repo.path()), "main");
    }

    #[test]
    fn commit_on_finish_commits_only_when_dirty() {
        let repo = repo();
        let p = repo.path();
        on_finish(OnFinish::Commit, p, "t", "Nothing to do").unwrap();
        assert_eq!(
            git(p, &["rev-list", "--count", "HEAD"]).unwrap().trim(),
            "1"
        );

        std::fs::write(p.join("new.txt"), "hi").unwrap();
        on_finish(OnFinish::Commit, p, "t", "Add a file\n\nWith details.").unwrap();
        assert!(!has_changes(p).unwrap());
        let log = git(p, &["log", "-1", "--format=%B"]).unwrap();
        assert!(log.starts_with("Add a file\n\nAdd a file\n\nWith details."));
        assert!(log.contains("lf task: t"));
    }

    #[test]
    fn commit_message_shapes() {
        assert_eq!(commit_message("t", "Short one"), "Short one");
        let long = "x".repeat(100);
        let subject = commit_message("t", &long)
            .lines()
            .next()
            .unwrap()
            .to_string();
        assert_eq!(subject.chars().count(), 72);
        assert!(subject.ends_with('…'));
    }
}
