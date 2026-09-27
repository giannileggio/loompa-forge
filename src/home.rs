use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The single folder holding everything loompa-forge owns.
///
/// Resolved from `--home`, then `$LF_HOME`, then `~/.loompa-forge`.
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn resolve(flag: Option<PathBuf>) -> Result<Self> {
        let root = match flag.or_else(|| std::env::var_os("LF_HOME").map(PathBuf::from)) {
            Some(p) => expand_tilde(&p),
            None => home_dir()?.join(".loompa-forge"),
        };
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn tasks(&self) -> PathBuf {
        self.root.join("tasks")
    }

    pub fn schedules(&self) -> PathBuf {
        self.root.join("schedules")
    }

    pub fn archive(&self) -> PathBuf {
        self.root.join("archive")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn worktrees(&self) -> PathBuf {
        self.root.join("worktrees")
    }

    pub fn dirs(&self) -> [PathBuf; 5] {
        [
            self.tasks(),
            self.schedules(),
            self.archive(),
            self.logs(),
            self.worktrees(),
        ]
    }

    /// Fails with a helpful message if `lf init` has not been run.
    pub fn ensure_initialized(&self) -> Result<()> {
        if !self.tasks().is_dir() {
            bail!(
                "{} is not initialized; run `lf init` first",
                self.root.display()
            );
        }
        Ok(())
    }
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("$HOME is not set")
}

/// Expands a leading `~` to `$HOME`. Other paths are returned unchanged.
pub fn expand_tilde(path: &Path) -> PathBuf {
    match path.strip_prefix("~") {
        Ok(rest) => match home_dir() {
            Ok(home) => home.join(rest),
            Err(_) => path.to_path_buf(),
        },
        Err(_) => path.to_path_buf(),
    }
}

/// Inverse of [`expand_tilde`], for display.
pub fn contract_tilde(path: &Path) -> String {
    if let Ok(home) = home_dir()
        && let Ok(rest) = path.strip_prefix(&home)
    {
        return Path::new("~").join(rest).display().to_string();
    }
    path.display().to_string()
}
