# loompa-forge

Queue and run coding-agent tasks defined as Markdown files.

- **Tasks** are `.md` files: YAML frontmatter says where and how to run
  (repo, branch, worktree, agent, model, schedule...), and the body is the
  prompt.
- **Schedules** are task templates with a cron expression. loompa-forge
  enqueues a new task each time one fires.
- **`lf`** runs pending tasks in tmux, tracks their status in the file, and
  moves finished tasks to `archive/`.

Because the format is plain Markdown, you can write tasks by hand, with
`lf add`, or by asking an agent: open it in `~/.loompa-forge` and say "add a
task to fix the login redirect in ~/Projects/myapp tonight, open a PR". `lf
init` puts an `AGENTS.md` (plus a `CLAUDE.md` that imports it) and an
`lf-tasks` skill there, which teach the agent to write a self-contained prompt
and queue it with `lf add`.

## Status

Working now: `init`, `add`, `ls`, `validate`, `lf run` (the runner and
scheduler: tmux windows, git worktrees, retries, timeouts, `on_finish`),
`done|fail|cancel|retry|attach`, `clean`, and the agent skill for writing
tasks.

## Install

```sh
cargo install --path .
lf init            # creates ~/.loompa-forge (override with $LF_HOME or --home)
```

## Usage

```sh
lf add --repo ~/Projects/myapp --on-finish pr --in 2h \
       --prompt "Fix the redirect loop after login"
echo "Update deps and run tests" | lf add --repo ~/Projects/myapp --model claude-opus-5-5

lf ls                # queue
lf ls --schedules    # schedules with their next run
lf ls --archive      # finished tasks
lf validate          # check every task and schedule file

lf run               # run the queue until Ctrl-C (rescans every poll_interval)
lf run --once        # one pass: enqueue due schedules, reap finished tasks, start new ones
lf attach [id]       # watch running tasks in tmux, one window each

lf done [id]         # finish a task: runs on_finish, archives it
lf fail [id] -r why  # give up on it, without retrying
lf cancel [id]       # drop a pending or running task
lf retry <id>        # requeue a failed/cancelled/needs_review task

lf clean             # remove worktrees of done tasks (-n: dry run)
lf clean --all --older-than 7d   # ...and of failed/cancelled ones, a week on
```

Inside a task's window, `id` defaults to that task, so an interactive
session ends with a plain `lf done` (or `! lf done` from the agent's prompt).

`lf run` starts each task in a window of the `loompa` tmux session, inside
`~/.loompa-forge/worktrees/<id>` (a git worktree on the task's branch) unless
`worktree: false`. It respects `max_parallel` and `max_parallel_per_repo`,
retries failures after `retry_delay`, kills attempts that exceed `timeout`,
runs `on_finish` on success, and moves finished tasks to `archive/`. Each
attempt's output is saved to `logs/<id>.<attempt>.log`.

See [docs/FORMAT.md](docs/FORMAT.md) for the file formats.
