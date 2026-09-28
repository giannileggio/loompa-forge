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
fn logs_reads_a_saved_attempt_via_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "t1", "do it");

    let out = failed(run(lf(home.path()).args(["logs", "t1"])));
    assert!(stderr(&out).contains("hasn't started"));

    // Fake a finished attempt: bump `attempts` on the task file and write
    // the log `lf run` would have saved.
    let task_path = home.path().join("tasks/t1.md");
    let content = std::fs::read_to_string(&task_path).unwrap();
    std::fs::write(
        &task_path,
        content.replacen("---\n", "---\nattempts: 1\n", 1),
    )
    .unwrap();
    std::fs::write(home.path().join("logs/t1.1.log"), "hello from the agent\n").unwrap();

    let out = stdout(&ok(run(lf(home.path()).args(["logs", "t1"]))));
    assert_eq!(out, "hello from the agent\n");

    let out = failed(run(lf(home.path()).args(["logs", "t1", "--attempt", "2"])));
    assert!(stderr(&out).contains("1 attempt"));
}

#[test]
fn commands_fail_clearly_before_init() {
    let home = tempfile::tempdir().unwrap();

    let out = failed(run(lf(home.path()).arg("ls")));
    assert!(stderr(&out).contains("not initialized"));
}

#[test]
fn ls_watch_refreshes_until_killed() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "watched", "do it");

    let mut child = lf(home.path())
        .args(["ls", "--watch", "--interval", "20ms"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();

    let text = stdout(&out);
    assert!(
        text.matches("watched").count() >= 2,
        "expected multiple refreshes, got:\n{text}"
    );
    assert!(text.contains("\x1B[2J\x1B[H"));
}

/// A raw GET over TCP (no HTTP client dependency), retrying until the
/// server is up. Panics if it never comes up within a few seconds.
fn http_get(port: u16, path: &str) -> String {
    use std::io::Read as _;
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                write!(
                    stream,
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut resp = String::new();
                stream.read_to_string(&mut resp).unwrap();
                return resp;
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(30)),
            Err(e) => panic!("could not connect to 127.0.0.1:{port}: {e}"),
        }
    }
}

#[test]
fn web_serves_the_dashboard_and_state() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    init(home.path());
    add(home.path(), repo.path(), "w1", "do it");

    let port = 18337u16;
    let mut child = lf(home.path())
        .args(["web", "--port", &port.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let index = http_get(port, "/");
    assert!(index.starts_with("HTTP/1.1 200"));
    assert!(index.contains("lf web"));

    let state = http_get(port, "/api/state");
    assert!(state.starts_with("HTTP/1.1 200"));
    assert!(state.contains("\"w1\""));
    assert!(state.contains("\"pending\":1"));

    let missing = http_get(port, "/nope");
    assert!(missing.starts_with("HTTP/1.1 404"));

    child.kill().unwrap();
    child.wait().unwrap();
}
