mod cmd;
mod config;
mod control;
mod frontmatter;
mod git;
mod home;
mod run;
mod schedule;
mod spec;
mod task;
mod tmux;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::home::Home;

/// Queue and run coding-agent tasks defined as Markdown files.
#[derive(Parser)]
#[command(name = "lf", version)]
struct Cli {
    /// Data folder [default: $LF_HOME or ~/.loompa-forge]
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create the data folder and a default config.toml.
    Init,
    /// List tasks (or archived tasks, or schedules).
    Ls {
        #[arg(long, conflicts_with = "schedules")]
        archive: bool,
        #[arg(long)]
        schedules: bool,
    },
    /// Create a new task file.
    Add(Box<cmd::AddArgs>),
    /// Check task/schedule files against the format. Checks all if none given.
    Validate { files: Vec<PathBuf> },
    /// Enqueue due schedules and run pending tasks in tmux, until interrupted.
    Run {
        /// Do a single pass and exit.
        #[arg(long)]
        once: bool,
    },
    /// Mark a task done: ends its session, runs on_finish, archives it.
    ///
    /// For running, needs_review or failed tasks. ID defaults to $LF_TASK_ID,
    /// which is set inside each task's window.
    Done { id: Option<String> },
    /// Mark a running or needs_review task failed, without retrying.
    Fail {
        id: Option<String>,
        /// Recorded as the task's `error`.
        #[arg(long, short)]
        reason: Option<String>,
    },
    /// Cancel a pending or running task. on_finish doesn't run.
    Cancel { id: Option<String> },
    /// Requeue a failed, cancelled or needs_review task, in the same worktree.
    Retry { id: String },
    /// Attach to a running task's tmux window (or the whole session).
    Attach { id: Option<String> },
    /// Launch a task's agent in the current process (used by `lf run`).
    #[command(hide = true)]
    Exec {
        id: String,
        #[arg(long)]
        status_file: PathBuf,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let home = Home::resolve(cli.home)?;
    match cli.command {
        Command::Init => cmd::init(&home)?,
        Command::Ls { archive, schedules } => cmd::ls(&home, archive, schedules)?,
        Command::Add(args) => cmd::add(&home, *args)?,
        Command::Validate { files } => return cmd::validate(&home, files),
        Command::Run { once } => run::run(&home, once)?,
        Command::Done { id } => control::done(&home, id)?,
        Command::Fail { id, reason } => control::fail(&home, id, reason)?,
        Command::Cancel { id } => control::cancel(&home, id)?,
        Command::Retry { id } => control::retry(&home, &id)?,
        Command::Attach { id } => control::attach(&home, id)?,
        Command::Exec { id, status_file } => return run::exec(&home, &id, &status_file),
    }
    Ok(ExitCode::SUCCESS)
}
