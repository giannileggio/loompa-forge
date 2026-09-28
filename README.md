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
CLI and opencode are built in, and others are a few lines of config.

Because the format is plain Markdown, you can write tasks by hand, with
`lf add`, or by asking an agent: open it in `~/.loompa-forge` and say "add a
task to fix the login redirect in ~/Projects/myapp tonight, open a PR". `lf
init` puts an `AGENTS.md` and an `lf-tasks` skill there, which teach the agent
to write a self-contained prompt and queue it with `lf add`. It can also link
the skill into your global skill folders, so you can queue work from any repo.

## Status

Working now: `init`, `add`, `ls`, `validate`, `lf run` (the runner and
scheduler: tmux windows, git worktrees, retries, timeouts, `on_finish`),
`done|fail|cancel|retry|attach`, `logs`, `clean`, `web` (a read-only
dashboard), and the agent skill for writing tasks.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/giannileggio/loompa-forge/releases/latest/download/loompa-forge-installer.sh | sh
lf init            # creates ~/.loompa-forge (override with $LF_HOME or --home);
                   # asks for the default agent and whether to install the skill globally
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
lf add --repo ~/Projects/myapp --on-finish pr --in 2h \
       --prompt "Fix the redirect loop after login"
echo "Update deps and run tests" | lf add --repo ~/Projects/myapp --agent codex

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

`lf web` serves a local, read-only dashboard (queue, running, archive and
schedules, refreshed every couple of seconds) — a Sidekiq-style view of the
same state `lf ls` prints. It only reads task files; use `lf` itself to act
on a task.

See [docs/FORMAT.md](docs/FORMAT.md) for the file formats.
