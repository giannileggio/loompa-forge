# loompa-forge

Queue and run coding-agent tasks defined as Markdown files.

- **Tasks** are `.md` files: YAML frontmatter says where and how to run
  (repo, branch, worktree, agent, model, schedule...), and the body is the
  prompt.
- **Schedules** are task templates with a cron expression. loompa-forge
  enqueues a new task each time one fires.
- **`lf`** runs pending tasks in tmux, tracks their status in the file, and
  moves finished tasks to `archive/`.

It works with any CLI coding agent: presets for Claude Code, Codex, Gemini
CLI, opencode and Pi are built in, and others are a few lines of config.

Because the format is plain Markdown, you can write tasks by hand, with
`lf add`, or by asking an agent: open it in `~/.loompa-forge` and say "add a
task to fix the login redirect in ~/Projects/myapp tonight, open a PR". `lf
init` puts an `AGENTS.md` and an `lf-tasks` skill there, which teach the agent
to write a self-contained prompt and queue it with `lf add`. It can also link
the skill into your global skill folders, so you can queue work from any repo.

## Quick start

```sh
# 1. install (Linux or macOS; needs git and tmux)
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/giannileggio/loompa-forge/releases/latest/download/loompa-forge-installer.sh | sh

# 2. queue some work, from inside your project
cd ~/Projects/myapp
lf add "Fix the redirect loop after login"

# 3. run it (starts the runner and opens the dashboard)
lf start
```

That's all: the first command you run sets up `~/.loompa-forge` and picks the
coding agent it finds on your `PATH`, and `lf add` works on the git repo you're
standing in. In the dashboard, **+ New task** queues more work without the
terminal. Run `lf` on its own any time to see where things stand and what to
try next, and `lf doctor` if something isn't running.

## Status

Working now: `init`, `add`, `ls`, `validate`, `start`, `lf run` (the runner and
scheduler: tmux windows, git worktrees, retries, timeouts, `on_finish`),
`done|fail|cancel|retry|attach`, `logs`, `clean`, `web` (a dashboard you can
also act from), `status`/`doctor`/`service` (keeping the runner healthy), and
the agent skill for writing tasks.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/giannileggio/loompa-forge/releases/latest/download/loompa-forge-installer.sh | sh
lf init            # optional: the first `lf add`/`lf start` sets up ~/.loompa-forge by
                   # itself (override with $LF_HOME or --home). `lf init` also asks
                   # whether to install the skill globally, so agents can queue tasks
```

Prebuilt binaries are published for Linux and macOS (x86_64 and arm64); the
script above fetches the right one and puts `lf` on your `PATH`. It also
installs `loompa-forge-update`, so `loompa-forge-update` upgrades an
existing install, and `lf` itself prints a one-line notice when a newer
version is out (set `LF_NO_UPDATE_CHECK=1` to turn that off).

Building from source instead:

```sh
cargo install --path .
```

## Usage

```sh
lf add "Fix the redirect loop after login"   # in the repo you're in; asks if you leave the text out
lf add --repo ~/Projects/myapp --on-finish pr --in 2h \
       "Fix the redirect loop after login"
echo "Update deps and run tests" | lf add --repo ~/Projects/myapp --agent codex

lf start             # the runner and the dashboard together (--no-open: no browser)

lf ls                # queue
lf ls --schedules    # schedules with their next run
lf ls --archive      # finished tasks
lf ls --watch        # ...refreshed every --interval (default 2s) until Ctrl-C
lf validate          # check every task and schedule file

lf run               # run the queue until Ctrl-C (rescans every poll_interval)
lf run --once        # one pass: enqueue due schedules, reap finished tasks, start new ones
lf attach [id]       # watch running tasks in tmux, one window each
lf logs <id>         # print a task's output: live if running, else the saved log
lf logs <id> --attempt 1   # an earlier attempt, instead of the latest

lf done [id]         # finish a task: runs on_finish, archives it
lf fail [id] -r why  # give up on it, without retrying
lf cancel [id]       # drop a pending or running task
lf retry <id>        # requeue a failed/cancelled/needs_review task

lf clean             # remove worktrees of done tasks (-n: dry run)
lf clean --all --older-than 7d   # ...and of failed/cancelled ones, a week on
lf clean --archive --all --older-than 30d   # ...and their archived task files/logs

lf status            # is the runner alive? queue, spend, failing schedules
lf doctor            # check tmux/git/agents/config/task files; non-zero on problems
lf service systemd   # print a unit that keeps `lf run` running (or: launchd)

lf web               # dashboard at http://127.0.0.1:7433, until Ctrl-C
lf web --port 8080   # a different port
```

Inside a task's window, `id` defaults to that task, so an interactive
session ends with a plain `lf done`, run by you or by the agent.

`lf run` starts each task in a window of the `loompa` tmux session, inside
`~/.loompa-forge/worktrees/<id>` (a git worktree on the task's branch) unless
`worktree: false`. It respects `max_parallel` and `max_parallel_per_repo`,
retries failures after `retry_delay`, kills attempts that exceed `timeout`,
runs `on_finish` on success, and moves finished tasks to `archive/`. Each
attempt's output is saved to `logs/<id>.<attempt>.log`.

## Keeping it running

`lf run` is a plain foreground process; `lf service systemd` (Linux) or
`lf service launchd` (macOS) prints a unit file, with install instructions,
that starts it at login and restarts it if it dies. Only one runner per home
is allowed. `lf status` shows whether it's alive and when its last pass was
(it warns if the runner looks stuck), and `lf run` also appends to
`runner.log` in the home folder.

What `lf` does so that a bad day doesn't lose work:

- **Crash-safe state.** Task, schedule and exit files are replaced atomically,
  never left half-written.
- **Nothing can hang it.** `git`, `gh` and `tmux` calls have timeouts and
  can't prompt; `on_finish` (push, PR) runs outside the lock the other
  commands share, and waiting for that lock gives up after two minutes.
- **Reboots aren't failures.** A task whose tmux window vanished is requeued
  at once without using a retry. Headless output is streamed to the log file
  as it's produced, so it survives tmux dying too.
- **Timeouts really stop the agent**: SIGTERM to its process group, then
  SIGKILL.
- **Guard rails.** A schedule won't enqueue a new run while its last one is
  still queued (unless `allow_overlap: true`); `daily_budget_usd` stops new
  tasks once the day's reported cost reaches it.
- **Safe git handling.** A reused worktree must belong to the repo, `lf`
  won't switch your checkout's branch over uncommitted changes, and
  `on_finish: pr` won't open a second PR for a branch that has one.

`lf web` serves a local dashboard (queue, running, archive and schedules,
refreshed every couple of seconds) — a Sidekiq-style view of the same state
`lf ls` prints. **+ New task** queues work from a form (what to do, which
project, when to start, what to do with the result), calling the same code as
`lf add`; `lf start` is `lf web` plus a runner that stops with it. Each task row offers the buttons that make sense for its
status (done, fail, cancel, retry) plus a log viewer, calling the same code
as `lf done|fail|cancel|retry` and `lf logs`. It only binds to
`127.0.0.1`, so anyone who can reach it could already run `lf` locally
themselves; there's no login. But a web page you visit can send requests to
`127.0.0.1` too, so the server only answers loopback `Host` names (against DNS
rebinding) and accepts actions only with a random per-run token that just the
dashboard page itself can read (against cross-site forgery). A task or
schedule file that can't be parsed shows up in a warning box instead of
breaking the page.

See [docs/FORMAT.md](docs/FORMAT.md) for the file formats.

See [TODO.md](TODO.md) for open work and ideas.
