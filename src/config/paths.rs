//! Resolution of on-disk locations for configuration, session and cache data.
//!
//! Every path can be redirected with `TGFWD_HOME`, which is what the test suite
//! uses and what lets a user keep several independent accounts side by side:
//!
//! ```sh
//! TGFWD_HOME=~/.tgfwd-work tgfwd start
//! ```

use std::env;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Context, Result, eyre};
use directories::ProjectDirs;

/// Environment variable that overrides every other path decision.
const HOME_ENV: &str = "TGFWD_HOME";

/// Resolved locations of everything this tool persists.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Directory holding the configuration file.
    config_dir: PathBuf,
    /// Directory holding the session file and other private state.
    data_dir: PathBuf,
    /// Directory holding snapshotted media. Safe to delete at any time.
    cache_dir: PathBuf,
}

impl Paths {
    /// Resolve paths from the environment, falling back to platform conventions.
    ///
    /// When `TGFWD_HOME` is set, all three directories live under it, which keeps
    /// a profile self-contained and trivially movable.
    pub fn resolve() -> Result<Self> {
        if let Some(home) = env::var_os(HOME_ENV) {
            return Ok(Self::from_root(home));
        }

        let dirs = ProjectDirs::from("", "", "tgfwd").ok_or_else(|| {
            eyre!(
                "could not determine a home directory for this platform; \
                 set {HOME_ENV} to choose one explicitly"
            )
        })?;

        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// Lay every directory out under a single root.
    ///
    /// This is what `TGFWD_HOME` selects, and it keeps a profile self-contained.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            config_dir: root,
        }
    }

    /// The TOML configuration file. May not exist yet.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    /// The session file holding the authorization key.
    ///
    /// This is a live credential: whoever holds it is logged into the account.
    pub fn session_file(&self) -> PathBuf {
        self.data_dir.join("session.json")
    }

    /// Directory for snapshotted media, used when a source message is deleted
    /// before it could be delivered everywhere.
    pub fn media_cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Create every directory this tool writes to.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.config_dir, &self.data_dir, &self.cache_dir] {
            fs_err::create_dir_all(dir)
                .wrap_err_with(|| format!("failed to create directory {}", dir.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `resolve()` is deliberately a thin wrapper over `from_root` so that the
    // interesting behaviour can be tested without mutating process-global
    // environment state, which is both unsafe and racy across parallel tests.

    #[test]
    fn a_root_keeps_everything_under_one_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(tmp.path());

        assert!(paths.config_file().starts_with(tmp.path()));
        assert!(paths.session_file().starts_with(tmp.path()));
        assert!(paths.media_cache_dir().starts_with(tmp.path()));
    }

    #[test]
    fn ensure_dirs_creates_every_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(tmp.path().join("nested"));
        paths.ensure_dirs().unwrap();

        assert!(paths.config_file().parent().unwrap().is_dir());
        assert!(paths.session_file().parent().unwrap().is_dir());
        assert!(paths.media_cache_dir().is_dir());
    }
}
