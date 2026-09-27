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
`lf add`, or by asking an agent to create them.

## Status

Early scaffold. Working now: `init`, `add`, `ls`, `validate`.
Next: the runner (`lf run`, tmux, worktrees), the scheduler, `lf done|fail|cancel|retry|attach`,
and an agent skill for authoring tasks.

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
```

See [docs/FORMAT.md](docs/FORMAT.md) for the file formats.
