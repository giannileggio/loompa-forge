use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, FixedOffset, Local};

use crate::config::{Config, DEFAULT_CONFIG_TOML};
use crate::frontmatter;
use crate::home::{Home, contract_tilde, expand_tilde};
use crate::schedule::Schedule;
use crate::spec::{Mode, OnFinish, TaskSpec, slugify, validate_id};
use crate::task::{Task, now};

const AGENTS_MD: &str = include_str!("../assets/AGENTS.md");
const SKILL_MD: &str = include_str!("../assets/SKILL.md");
const FORMAT_MD: &str = include_str!("../docs/FORMAT.md");
const GENERATED: &str = "<!-- Written by `lf init`, which overwrites it: don't edit. -->\n\n";

pub fn init(home: &Home) -> Result<()> {
    for dir in home.dirs() {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // The user's to edit: written once.
    write_if_missing(&home.config_path(), DEFAULT_CONFIG_TOML)?;
    write_if_missing(&home.root().join("AGENTS.md"), AGENTS_MD)?;
    write_if_missing(&home.root().join("CLAUDE.md"), "@AGENTS.md\n")?;
    // lf's own: refreshed so they match this binary.
    overwrite(
        &home.root().join("FORMAT.md"),
        &format!("{GENERATED}{FORMAT_MD}"),
    )?;
    overwrite(&home.skill_path(), &with_generated_note(SKILL_MD))?;
    println!("initialized {}", home.root().display());
    Ok(())
}

fn write_if_missing(path: &Path, contents: &str) -> Result<()> {
    if path.exists() {
        println!("kept existing {}", path.display());
    } else {
        std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

fn overwrite(path: &Path, contents: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    println!("wrote {}", path.display());
    Ok(())
}

/// Puts the note after the skill's frontmatter, which must stay first.
fn with_generated_note(skill: &str) -> String {
    let body_start = skill
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n"))
        .map(|i| i + "---\n".len() + "\n---\n".len())
        .expect("assets/SKILL.md starts with frontmatter");
    let (frontmatter, body) = skill.split_at(body_start);
    format!("{frontmatter}\n{GENERATED}{}", body.trim_start())
}

pub fn ls(home: &Home, archive: bool, schedules: bool) -> Result<()> {
    home.ensure_initialized()?;
    let config = Config::load(&home.config_path())?;
    if schedules {
        return ls_schedules(home, &config);
    }

    let dir = if archive {
        home.archive()
    } else {
        home.tasks()
    };
    let mut tasks = Vec::new();
    for path in md_files(&dir)? {
        match Task::load(&path) {
            Ok(t) => tasks.push(t),
            Err(e) => eprintln!("warning: skipping {}: {e:#}", path.display()),
        }
    }
    tasks.sort_by_key(|t| (t.scheduled_at.or(t.created_at), t.id.clone()));

    let mut rows = vec![row(["ID", "STATUS", "WHEN", "REPO", "BRANCH", "AGENT"])];
    for t in &tasks {
        let eff = t.spec.resolve(&config.defaults);
        let when = match (t.finished_at, t.started_at, t.scheduled_at) {
            (Some(at), _, _) | (None, Some(at), _) | (None, None, Some(at)) => fmt_time(at),
            _ => "asap".into(),
        };
        rows.push(vec![
            t.id.clone(),
            t.status.as_str().into(),
            when,
            contract_tilde(&eff.repo),
            t.effective_branch(&config).unwrap_or_else(|| "-".into()),
            format!("{}/{}", eff.agent, eff.model),
        ]);
    }
    print_table(&rows);
    Ok(())
}

fn ls_schedules(home: &Home, config: &Config) -> Result<()> {
    let mut rows = vec![row(["ID", "ENABLED", "CRON", "NEXT", "REPO"])];
    for path in md_files(&home.schedules())? {
        let s = match Schedule::load(&path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("warning: skipping {}: {e:#}", path.display());
                continue;
            }
        };
        let next = match s.next_after(Local::now()) {
            Ok(t) if s.enabled => fmt_time(t.fixed_offset()),
            Ok(_) => "-".into(),
            Err(_) => "invalid".into(),
        };
        rows.push(vec![
            s.id.clone(),
            if s.enabled { "yes" } else { "no" }.into(),
            s.cron.clone(),
            next,
            contract_tilde(&s.spec.resolve(&config.defaults).repo),
        ]);
    }
    print_table(&rows);
    Ok(())
}

#[derive(clap::Args)]
pub struct AddArgs {
    /// Repository the task runs in.
    #[arg(long)]
    repo: PathBuf,
    /// Task id (also the file name). Derived from the prompt if omitted.
    #[arg(long)]
    id: Option<String>,
    /// The prompt. Read from stdin if omitted.
    #[arg(long, short)]
    prompt: Option<String>,
    #[arg(long)]
    branch: Option<String>,
    /// Run directly in the repo instead of a dedicated worktree.
    #[arg(long)]
    no_worktree: bool,
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    #[arg(long, value_enum)]
    on_finish: Option<OnFinish>,
    #[arg(long)]
    retries: Option<u32>,
    /// e.g. 5m, 1h
    #[arg(long, value_parser = humantime::parse_duration)]
    retry_delay: Option<Duration>,
    /// e.g. 30m, 2h
    #[arg(long, value_parser = humantime::parse_duration)]
    timeout: Option<Duration>,
    /// Start time, RFC 3339 (2026-09-28T02:00:00+02:00).
    #[arg(long, conflicts_with = "delay")]
    at: Option<DateTime<FixedOffset>>,
    /// Start after a delay, e.g. 2h.
    #[arg(long = "in", value_parser = humantime::parse_duration)]
    delay: Option<Duration>,
    /// Who created the task (e.g. cli, agent).
    #[arg(long, default_value = "cli")]
    created_by: String,
}

pub fn add(home: &Home, args: AddArgs) -> Result<()> {
    home.ensure_initialized()?;
    let config = Config::load(&home.config_path())?;

    let prompt = match args.prompt {
        Some(p) => p,
        None if std::io::stdin().is_terminal() => {
            bail!("no prompt: pass --prompt or pipe it on stdin")
        }
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        bail!("the prompt is empty");
    }

    let id = match args.id {
        Some(id) => {
            validate_id(&id).map_err(anyhow::Error::msg)?;
            if id_taken(home, &id) {
                bail!("a task with id `{id}` already exists");
            }
            id
        }
        None => unique_id(
            home,
            &slugify(prompt.lines().next().unwrap_or_default(), 48),
        ),
    };

    let repo = std::path::absolute(expand_tilde(&args.repo))?;
    let spec = TaskSpec {
        repo: PathBuf::from(contract_tilde(&repo)),
        branch: args.branch,
        worktree: args.no_worktree.then_some(false),
        agent: args.agent,
        model: args.model,
        mode: args.mode,
        on_finish: args.on_finish,
        retries: args.retries,
        retry_delay: args.retry_delay,
        timeout: args.timeout,
    };
    let mut task = Task::new(id, spec, prompt);
    task.created_by = Some(args.created_by);
    task.scheduled_at = match (args.at, args.delay) {
        (Some(at), _) => Some(at),
        (None, Some(delay)) => Some(now() + delay),
        (None, None) => None,
    };

    let problems = task.problems(&config, None);
    if !problems.is_empty() {
        bail!("invalid task:\n  - {}", problems.join("\n  - "));
    }
    let path = home.tasks().join(format!("{}.md", task.id));
    task.create(&path)?;
    println!("{}", path.display());
    Ok(())
}

fn id_taken(home: &Home, id: &str) -> bool {
    let file = format!("{id}.md");
    home.tasks().join(&file).exists() || home.archive().join(&file).exists()
}

pub fn unique_id(home: &Home, base: &str) -> String {
    (1..)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|id| !id_taken(home, id))
        .expect("infinite iterator")
}

/// Validates the given files, or every task and schedule if none are given.
/// Prints one line per file so agents can parse the result.
pub fn validate(home: &Home, files: Vec<PathBuf>) -> Result<ExitCode> {
    let config = Config::load(&home.config_path())?;
    let files = if files.is_empty() {
        home.ensure_initialized()?;
        let mut all = md_files(&home.tasks())?;
        all.extend(md_files(&home.schedules())?);
        all
    } else {
        files
    };

    let mut failed = 0;
    for path in &files {
        let problems = match validate_file(&config, path) {
            Ok(p) => p,
            Err(e) => vec![format!("{e:#}")],
        };
        if problems.is_empty() {
            println!("ok     {}", path.display());
        } else {
            failed += 1;
            println!("error  {}", path.display());
            for p in problems {
                println!("       - {p}");
            }
        }
    }
    println!("{} file(s) checked, {failed} with errors", files.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn validate_file(config: &Config, path: &Path) -> Result<Vec<String>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(if frontmatter::has_key(&text, "cron") {
        Schedule::parse(&text)?.problems(config, Some(path))
    } else {
        Task::parse(&text)?.problems(config, Some(path))
    })
}

pub fn md_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    Ok(files)
}

fn fmt_time(t: DateTime<FixedOffset>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn row<const N: usize>(cells: [&str; N]) -> Vec<String> {
    cells.iter().map(|s| s.to_string()).collect()
}

fn print_table(rows: &[Vec<String>]) {
    if rows.len() == 1 {
        println!("(none)");
        return;
    }
    let cols = rows[0].len();
    let widths: Vec<usize> = (0..cols)
        .map(|c| rows.iter().map(|r| r[c].chars().count()).max().unwrap_or(0))
        .collect();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_keeps_user_files_and_refreshes_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::resolve(Some(dir.path().to_path_buf())).unwrap();
        init(&home).unwrap();

        let skill = std::fs::read_to_string(home.skill_path()).unwrap();
        assert!(skill.starts_with("---\nname: lf-tasks\n"));
        assert!(skill.contains("---\n\n<!-- Written by `lf init`"));
        assert!(frontmatter::has_key(&skill, "description"));

        let agents = dir.path().join("AGENTS.md");
        std::fs::write(&agents, "mine").unwrap();
        std::fs::write(home.skill_path(), "stale").unwrap();
        init(&home).unwrap();
        assert_eq!(std::fs::read_to_string(&agents).unwrap(), "mine");
        assert_eq!(std::fs::read_to_string(home.skill_path()).unwrap(), skill);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap(),
            "@AGENTS.md\n"
        );
    }
}
