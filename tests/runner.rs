//! End-to-end tests of the runner (`lf run`) against the built binary, with
//! a stub "agent" that just runs the task's prompt as a shell command.
//!
//! Each test gets its own home, its own git repo and its own tmux server
//! (via `TMUX_TMPDIR`), so nothing touches the real user's tmux or files.
//! Needs `tmux` and `git`, like the runner itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Env {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
    tmux: tempfile::TempDir,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("running git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

impl Env {
    fn new(extra_config: &str) -> Env {
        let env = Env {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
            tmux: tempfile::tempdir().unwrap(),
        };
        ok(env
            .lf()
            .args(["init", "--agent", "claude", "--no-global-skill"])
            .output()
            .unwrap());
        std::fs::write(
            env.home.path().join("config.toml"),
            format!(
                "[runner]\ntmux_session = \"lft\"\npoll_interval = \"1s\"\n{extra_config}\n\
                 [defaults]\nagent = \"stub\"\nretry_delay = \"1s\"\n\n\
                 [agents.stub]\nheadless = [\"sh\", \"-c\", \"{{prompt}}\"]\n\
                 interactive = [\"sh\", \"-c\", \"{{prompt}}\"]\n"
            ),
        )
        .unwrap();
        let repo = env.repo.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.email", "test@example.com"]);
        git(repo, &["config", "user.name", "Test"]);
        git(repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        env
    }

    fn lf(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_lf"));
        cmd.env("HOME", self.home.path())
            .env("TMUX_TMPDIR", self.tmux.path())
            .env_remove("TMUX")
            .env("LF_NO_UPDATE_CHECK", "1")
            .arg("--home")
            .arg(self.home.path())
            .stdin(Stdio::null());
        cmd
    }

    fn lf_ok(&self, args: &[&str]) -> Output {
        ok(self.lf().args(args).output().unwrap())
    }

    /// Queues a task running `prompt` in the test repo.
    fn add(&self, id: &str, prompt: &str, extra: &[&str]) {
        let repo = self.repo.path().display().to_string();
        let mut args = vec!["add", "--repo", &repo, "--id", id, "--prompt", prompt];
        args.extend(extra);
        self.lf_ok(&args);
    }

    /// One `lf run --once` pass.
    fn tick(&self) {
        self.lf_ok(&["run", "--once"]);
    }

    fn task_path(&self, id: &str) -> Option<PathBuf> {
        ["tasks", "archive"]
            .iter()
            .map(|d| self.home.path().join(d).join(format!("{id}.md")))
            .find(|p| p.exists())
    }

    /// A frontmatter value of the task, and whether it's archived.
    fn field(&self, id: &str, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.task_path(id)?).ok()?;
        text.lines()
            .skip(1)
            .take_while(|l| *l != "---")
            .find_map(|l| l.strip_prefix(&format!("{key}: ")).map(str::to_string))
    }

    fn archived(&self, id: &str) -> bool {
        self.home
            .path()
            .join("archive")
            .join(format!("{id}.md"))
            .exists()
    }

    fn worktree(&self, id: &str) -> PathBuf {
        self.home.path().join("worktrees").join(id)
    }

    /// Ticks until `id` is archived, and returns its final status.
    fn run_until_archived(&self, id: &str) -> String {
        wait_for(
            &format!("{id} to be archived"),
            Duration::from_secs(60),
            || {
                self.tick();
                self.archived(id)
            },
        );
        self.field(id, "status").unwrap()
    }

    fn tmux_kill_server(&self) {
        let _ = Command::new("tmux")
            .arg("kill-server")
            .env("TMUX_TMPDIR", self.tmux.path())
            .env_remove("TMUX")
            .output();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        self.tmux_kill_server();
    }
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "expected success, got {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn wait_for(what: &str, timeout: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Whether a process is still running (a zombie doesn't count).
fn alive(pid: &str) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", pid])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    !stat.trim().is_empty() && !stat.trim().starts_with('Z')
}

#[test]
fn a_task_runs_to_done_and_its_work_is_committed() {
    let env = Env::new("");
    env.add(
        "happy",
        "echo hello-from-agent; echo data > out.txt",
        &["--on-finish", "commit"],
    );

    assert_eq!(env.run_until_archived("happy"), "done");
    assert_eq!(env.field("happy", "attempts").as_deref(), Some("1"));

    let log = std::fs::read_to_string(env.home.path().join("logs/happy.1.log")).unwrap();
    assert!(log.contains("hello-from-agent"), "{log}");
    let wt = env.worktree("happy");
    assert_eq!(
        git(&wt, &["show", "--name-only", "--format=", "HEAD"]),
        "out.txt"
    );
    assert_eq!(git(&wt, &["branch", "--show-current"]), "lf/happy");
}

#[test]
fn a_failing_task_is_retried_then_archived_as_failed() {
    let env = Env::new("");
    env.add("flaky", "echo boom >&2; exit 3", &["--retries", "1"]);

    assert_eq!(env.run_until_archived("flaky"), "failed");
    assert_eq!(env.field("flaky", "attempts").as_deref(), Some("2"));
    assert_eq!(env.field("flaky", "exit_code").as_deref(), Some("3"));
    assert!(env.field("flaky", "error").unwrap().contains("status 3"));
    for attempt in [1, 2] {
        let log =
            std::fs::read_to_string(env.home.path().join(format!("logs/flaky.{attempt}.log")))
                .unwrap();
        assert!(log.contains("boom"), "stderr is logged too: {log}");
    }
}

#[test]
fn a_timeout_stops_the_agent_and_everything_it_started() {
    let env = Env::new("");
    env.add(
        "slow",
        "sleep 300 & echo $! > pid.txt; wait",
        &["--timeout", "2s", "--retries", "0"],
    );

    assert_eq!(env.run_until_archived("slow"), "failed");
    assert!(env.field("slow", "error").unwrap().contains("timed out"));
    let pid = std::fs::read_to_string(env.worktree("slow").join("pid.txt")).unwrap();
    wait_for("the sleep to be killed", Duration::from_secs(10), || {
        !alive(pid.trim())
    });
}

#[test]
fn losing_the_tmux_server_requeues_without_using_a_retry() {
    let env = Env::new("");
    env.add("survivor", "sleep 300", &["--retries", "0"]);

    env.tick();
    assert_eq!(env.field("survivor", "status").as_deref(), Some("running"));
    env.tmux_kill_server(); // like a reboot

    env.tick();
    assert_eq!(env.field("survivor", "status").as_deref(), Some("running"));
    assert_eq!(env.field("survivor", "interruptions").as_deref(), Some("1"));
    assert_eq!(env.field("survivor", "attempts").as_deref(), Some("2"));
    assert!(
        !env.archived("survivor"),
        "retries: 0, yet it wasn't failed"
    );

    env.lf_ok(&["cancel", "survivor"]);
}

#[test]
fn only_one_runner_per_home_and_status_notices() {
    let env = Env::new("");
    let status = |env: &Env| String::from_utf8_lossy(&env.lf_ok(&["status"]).stdout).into_owned();
    assert!(status(&env).contains("NOT running"));

    let mut runner = env
        .lf()
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for("the runner to show up", Duration::from_secs(10), || {
        status(&env).contains("runner     running")
    });

    let second = env.lf().args(["run", "--once"]).output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    runner.kill().unwrap();
    runner.wait().unwrap();
    assert!(
        status(&env).contains("NOT running"),
        "the claim dies with the runner"
    );
}

#[test]
fn doctor_checks_the_setup() {
    let env = Env::new("");
    let out = env.lf().arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("ok    tmux is installed"), "{text}");
    assert!(text.contains("agent `stub`"), "{text}");

    std::fs::write(env.home.path().join("tasks/bad.md"), "garbage").unwrap();
    let out = env.lf().arg("doctor").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("bad.md"));
}

/// Schedule file whose last firing was long ago, so the next pass enqueues.
fn due_schedule(env: &Env, extra: &str) {
    let long_ago = chrono::Local::now().fixed_offset() - chrono::Duration::hours(3);
    std::fs::write(
        env.home.path().join("schedules/tick.md"),
        format!(
            "---\nid: tick\ncron: \"* * * * *\"\nrepo: {}\nretries: 0\n{extra}\
             last_enqueued_at: {}\n---\n\nsleep 300\n",
            env.repo.path().display(),
            long_ago.to_rfc3339()
        ),
    )
    .unwrap();
}

fn scheduled_tasks(env: &Env) -> usize {
    std::fs::read_dir(env.home.path().join("tasks"))
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.unwrap().path()).ok())
        .filter(|t| t.contains("created_by: schedule:tick"))
        .count()
}

#[test]
fn a_schedule_does_not_pile_up_runs_unless_allowed() {
    let env = Env::new("");
    due_schedule(&env, "");
    env.tick();
    assert_eq!(scheduled_tasks(&env), 1);

    due_schedule(&env, ""); // another slot passed; the first run is still going
    env.tick();
    assert_eq!(
        scheduled_tasks(&env),
        1,
        "skipped while the last run is queued"
    );

    due_schedule(&env, "allow_overlap: true\n");
    env.tick();
    assert_eq!(scheduled_tasks(&env), 2);
}

#[test]
fn the_daily_budget_stops_new_tasks_from_starting() {
    let env = Env::new("daily_budget_usd = 1.0");
    std::fs::write(
        env.home.path().join("archive/spent.md"),
        format!(
            "---\nid: spent\nstatus: done\nrepo: {}\nfinished_at: {}\ncost_usd: 5.0\n---\n\nx\n",
            env.repo.path().display(),
            chrono::Local::now().to_rfc3339()
        ),
    )
    .unwrap();
    env.add("waiting", "true", &[]);

    env.tick();
    assert_eq!(env.field("waiting", "status").as_deref(), Some("pending"));
    let status = String::from_utf8_lossy(&env.lf_ok(&["status"]).stdout).into_owned();
    assert!(status.contains("$5.0000 of $1.0000"), "{status}");
}

#[test]
fn a_slow_on_finish_does_not_block_other_commands() {
    let env = Env::new("");
    // A bare "origin" whose pre-push hook takes a while stands in for a slow network.
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    let repo = env.repo.path();
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            &origin.path().display().to_string(),
        ],
    );
    let hook = repo.join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nsleep 8\n").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    env.add("pushy", "echo work > f.txt", &["--on-finish", "push"]);
    env.add("bystander", "true", &["--in", "10h"]);

    env.tick(); // starts it
    wait_for("the agent to exit", Duration::from_secs(30), || {
        env.home.path().join("logs/pushy.1.exit").exists()
    });
    let mut finishing = env.lf().args(["run", "--once"]).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(1500)); // now inside the hook

    let started = Instant::now();
    env.lf_ok(&["cancel", "bystander"]); // needs the home lock
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "blocked behind on_finish"
    );
    assert!(
        finishing.try_wait().unwrap().is_none(),
        "on_finish still running"
    );

    assert!(finishing.wait().unwrap().success());
    assert_eq!(env.field("pushy", "status").as_deref(), Some("done"));
    assert_eq!(
        git(origin.path(), &["branch", "--list", "lf/pushy"]).trim(),
        "lf/pushy"
    );
}

/// A fake `gh` that records `pr create` calls; `pr view` succeeds or fails
/// as given. Returns the directory to put first on `$PATH`.
fn fake_gh(dir: &Path, pr_exists: bool) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(
        &gh,
        format!(
            "#!/bin/sh\ncase \"$2\" in\n  view) exit {};;\n  create) echo created >> \"{}\";;\nesac\n",
            if pr_exists { 0 } else { 1 },
            dir.join("gh-creates.log").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn finish_with_pr(pr_exists: bool) -> usize {
    let env = Env::new("");
    let origin = tempfile::tempdir().unwrap();
    git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(
        env.repo.path(),
        &[
            "remote",
            "add",
            "origin",
            &origin.path().display().to_string(),
        ],
    );
    let bin = fake_gh(env.home.path(), pr_exists);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    env.add("prtask", "echo work > f.txt", &["--on-finish", "pr"]);

    wait_for("prtask to be archived", Duration::from_secs(60), || {
        ok(env
            .lf()
            .env("PATH", &path)
            .args(["run", "--once"])
            .output()
            .unwrap());
        env.archived("prtask")
    });
    assert_eq!(env.field("prtask", "status").as_deref(), Some("done"));
    std::fs::read_to_string(env.home.path().join("gh-creates.log"))
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

#[test]
fn on_finish_pr_opens_a_pr_when_there_is_none() {
    assert_eq!(finish_with_pr(false), 1);
}

#[test]
fn on_finish_pr_does_not_open_a_second_pr_for_the_branch() {
    assert_eq!(finish_with_pr(true), 0);
}
