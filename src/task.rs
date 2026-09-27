use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, Local, SubsecRound};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::frontmatter;
use crate::spec::{SPEC_KEYS, TaskSpec, validate_id};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    /// An interactive session ended without signalling done/fail.
    NeedsReview,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pending => "pending",
            Status::Running => "running",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
            Status::NeedsReview => "needs_review",
        }
    }
}

/// One task file: `tasks/<id>.md`. The Markdown body is the prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    #[serde(default)]
    pub status: Status,
    #[serde(flatten)]
    pub spec: TaskSpec,
    /// Don't start before this time. Absent means as soon as possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<FixedOffset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<FixedOffset>>,
    /// Free text, e.g. `cli`, `agent`, `schedule:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,

    // Written by loompa-forge only.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<FixedOffset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<FixedOffset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    #[serde(skip)]
    pub prompt: String,
}

const TASK_KEYS: &[&str] = &[
    "id",
    "status",
    "scheduled_at",
    "created_at",
    "created_by",
    "attempts",
    "started_at",
    "finished_at",
    "exit_code",
    "tmux_window",
    "error",
];

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// Current local time, to the second (keeps the files readable).
pub fn now() -> DateTime<FixedOffset> {
    Local::now().fixed_offset().trunc_subsecs(0)
}

impl Task {
    pub fn new(id: String, spec: TaskSpec, prompt: String) -> Self {
        Self {
            id,
            status: Status::Pending,
            spec,
            scheduled_at: None,
            created_at: Some(now()),
            created_by: None,
            attempts: 0,
            started_at: None,
            finished_at: None,
            exit_code: None,
            tmux_window: None,
            error: None,
            prompt,
        }
    }

    pub fn parse(content: &str) -> Result<Self> {
        let allowed: Vec<&str> = TASK_KEYS.iter().chain(SPEC_KEYS).copied().collect();
        let (mut task, body): (Task, String) = frontmatter::parse(content, &allowed)?;
        task.prompt = body;
        Ok(task)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn to_markdown(&self) -> Result<String> {
        frontmatter::render(self, &self.prompt)
    }

    /// Writes the task, failing if the file already exists.
    pub fn create(&self, path: &Path) -> Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(self.to_markdown()?.as_bytes())?;
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_markdown()?)
            .with_context(|| format!("writing {}", path.display()))
    }

    /// Branch the task runs on: explicit, or `lf/<id>` when using a worktree.
    pub fn effective_branch(&self, config: &Config) -> Option<String> {
        let eff = self.spec.resolve(&config.defaults);
        eff.branch
            .or_else(|| eff.worktree.then(|| format!("lf/{}", self.id)))
    }

    /// Problems that make this task unrunnable. `path`, if given, is checked
    /// against the id.
    pub fn problems(&self, config: &Config, path: Option<&Path>) -> Vec<String> {
        let mut out = Vec::new();
        if let Err(e) = validate_id(&self.id) {
            out.push(e);
        }
        if let Some(stem) = path.and_then(|p| p.file_stem()).and_then(|s| s.to_str())
            && stem != self.id
        {
            out.push(format!(
                "file name `{stem}.md` does not match id `{}`",
                self.id
            ));
        }
        if self.prompt.is_empty() {
            out.push("empty prompt: write the instructions below the frontmatter".into());
        }
        out.extend(self.spec.problems(config));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
id: fix-login
repo: /tmp
branch: fix/login
on_finish: pr
retries: 3
timeout: 30m
scheduled_at: 2026-09-28T02:00:00+02:00
---

Fix the redirect loop.
";

    #[test]
    fn parses_sample() {
        let t = Task::parse(SAMPLE).unwrap();
        assert_eq!(t.id, "fix-login");
        assert_eq!(t.status, Status::Pending);
        assert_eq!(t.spec.retries, Some(3));
        assert_eq!(t.spec.timeout, Some(std::time::Duration::from_secs(1800)));
        assert_eq!(t.prompt, "Fix the redirect loop.");
        assert!(t.scheduled_at.is_some());
    }

    #[test]
    fn roundtrips() {
        let t = Task::parse(SAMPLE).unwrap();
        let again = Task::parse(&t.to_markdown().unwrap()).unwrap();
        assert_eq!(t, again);
    }

    #[test]
    fn rejects_unknown_field() {
        let bad = SAMPLE.replace("retries: 3", "retires: 3");
        assert!(Task::parse(&bad).is_err());
    }

    #[test]
    fn requires_repo() {
        let bad = SAMPLE.replace("repo: /tmp\n", "");
        assert!(Task::parse(&bad).is_err());
    }

    #[test]
    fn default_branch_for_worktrees() {
        let mut t = Task::parse(SAMPLE).unwrap();
        let c = Config::default();
        assert_eq!(t.effective_branch(&c).as_deref(), Some("fix/login"));
        t.spec.branch = None;
        assert_eq!(t.effective_branch(&c).as_deref(), Some("lf/fix-login"));
        t.spec.worktree = Some(false);
        assert_eq!(t.effective_branch(&c), None);
    }

    #[test]
    fn reports_mismatched_file_name() {
        let t = Task::parse(SAMPLE).unwrap();
        let problems = t.problems(&Config::default(), Some(Path::new("tasks/other.md")));
        assert!(problems.iter().any(|p| p.contains("does not match id")));
    }
}
