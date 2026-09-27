//! End-to-end tests against the built `lf` binary.
//!
//! The unit tests under `src/` cover each command's logic directly; these
//! instead go through `Cli::parse` and the real filesystem, to catch
//! problems in argument wiring (flags, conflicts, global `--home`) and in
//! how commands compose through the process boundary (exit codes, stdin,
//! files one command leaves for the next to read).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// A fresh `lf` invocation against `home`. `$HOME` is also pointed at it, so
/// the global-skill links `lf init` may touch land inside the temp dir
/// instead of the real user home.
fn lf(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lf"));
    cmd.env("HOME", home)
        .arg("--home")
        .arg(home)
        .stdin(Stdio::null());
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("spawning lf")
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout: {}\nstderr: {}",
        out.status,
        stdout(&out),
        stderr(&out)
    );
    out
}

fn failed(out: Output) -> Output {
    assert!(
        !out.status.success(),
        "expected failure, got success\nstdout: {}",
        stdout(&out)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn init(home: &Path) {
    ok(run(lf(home).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
}

fn add(home: &Path, repo: &Path, id: &str, prompt: &str) {
    ok(run(lf(home).args([
        "add",
        "--repo",
        &repo.display().to_string(),
        "--no-worktree",
        "--id",
        id,
        "--prompt",
        prompt,
    ])));
}

#[test]
fn init_creates_the_home_layout() {
    let home = tempfile::tempdir().unwrap();

    let out = ok(run(lf(home.path()).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
    assert!(stdout(&out).contains("wrote"));
    assert!(stdout(&out).contains("initialized"));
    assert!(home.path().join("config.toml").is_file());
    assert!(home.path().join("AGENTS.md").is_file());
    for dir in ["tasks", "schedules", "archive", "logs", "worktrees"] {
        assert!(home.path().join(dir).is_dir(), "missing {dir}/");
    }

    // Re-running keeps the user's config instead of overwriting it.
    let out = ok(run(lf(home.path()).args([
        "init",
        "--agent",
        "claude",
        "--no-global-skill",
    ])));
    assert!(stdout(&out).contains("kept existing"));
}

#[test]
fn add_ls_and_validate_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let out = ok(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "my-task",
        "--prompt",
        "do the thing",
    ])));
    assert!(stdout(&out).trim_end().ends_with("my-task.md"));

    let out = ok(run(lf(home.path()).arg("ls")));
    let listing = stdout(&out);
    assert!(listing.contains("my-task"));
    assert!(listing.contains("pending"));

    let out = ok(run(lf(home.path()).arg("validate")));
    let report = stdout(&out);
    assert!(report.contains("ok"));
    assert!(report.contains("1 file(s) checked, 0 with errors"));
}

#[test]
fn add_reads_the_prompt_from_stdin() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let mut child = lf(home.path())
        .args([
            "add",
            "--repo",
            &repo.path().display().to_string(),
            "--no-worktree",
            "--id",
            "from-stdin",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"  fix the thing  \n")
        .unwrap();
    let out = ok(child.wait_with_output().unwrap());
    assert!(stdout(&out).trim_end().ends_with("from-stdin.md"));

    let body = std::fs::read_to_string(home.path().join("tasks/from-stdin.md")).unwrap();
    assert!(body.trim_end().ends_with("fix the thing"));
}

#[test]
fn add_rejects_an_empty_prompt() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "empty",
    ])));
    assert!(stderr(&out).contains("prompt is empty"));
}

#[test]
fn add_rejects_a_missing_repo() {
    let home = tempfile::tempdir().unwrap();
    init(home.path());

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        "/no/such/repo",
        "--no-worktree",
        "--prompt",
        "x",
    ])));
    assert!(stderr(&out).contains("does not exist"));
}

#[test]
fn add_rejects_a_duplicate_id() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "dup", "first");

    let out = failed(run(lf(home.path()).args([
        "add",
        "--repo",
        &repo.path().display().to_string(),
        "--no-worktree",
        "--id",
        "dup",
        "--prompt",
        "second",
    ])));
    assert!(stderr(&out).contains("already exists"));
}

#[test]
fn cancel_then_retry_via_cli() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "t1", "do it");

    ok(run(lf(home.path()).args(["cancel", "t1"])));
    let archived = stdout(&ok(run(lf(home.path()).args(["ls", "--archive"]))));
    assert!(archived.contains("t1") && archived.contains("cancelled"));
    let pending = stdout(&ok(run(lf(home.path()).arg("ls"))));
    assert!(pending.contains("(none)"));

    ok(run(lf(home.path()).args(["retry", "t1"])));
    let pending = stdout(&ok(run(lf(home.path()).arg("ls"))));
    assert!(pending.contains("t1") && pending.contains("pending"));
    let archived = stdout(&ok(run(lf(home.path()).args(["ls", "--archive"]))));
    assert!(archived.contains("(none)"));
}

#[test]
fn commands_fail_clearly_before_init() {
    let home = tempfile::tempdir().unwrap();

    let out = failed(run(lf(home.path()).arg("ls")));
    assert!(stderr(&out).contains("not initialized"));
}
