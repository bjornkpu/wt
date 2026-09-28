use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("cannot find the home directory; set WT_HOME")]
    NoHome,
    #[error("{0}")]
    ConfigInvalid(String),
    #[error("cannot read {path}: {source}")]
    ConfigRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{file}: {source}")]
    ConfigParse {
        file: String,
        source: toml::de::Error,
    },
    #[error("cannot run git: {source}")]
    GitSpawn { source: std::io::Error },
    #[error("git {args} timed out after {timeout}s")]
    GitTimeout { args: String, timeout: u64 },
    #[error("git {args} left processes holding its output")]
    GitHeld { args: String },
    #[error("git {args}\n{message}")]
    Git { args: String, message: String },
    #[error("no worktree matching '{0}'")]
    NoWorktreeMatching(String),
    #[error("'{want}' matches {count} worktrees:\n  {listing}")]
    Ambiguous {
        want: String,
        count: usize,
        listing: String,
    },
    #[error("unresolved placeholder in template '{template}' -> '{out}'")]
    UnresolvedPlaceholder { template: String, out: String },
    #[error("{mode}: no branch or directory name to create a worktree from")]
    NoLeaf { mode: String },
    #[error("{mode}: nothing to check out - no branch and not detached")]
    NothingToCheckOut { mode: String },
    #[error("refusing to create outside {}: {}", root.display(), path.display())]
    OutsideRoot { root: PathBuf, path: PathBuf },
    #[error("{} is pinned to {old} but {mode} now resolves to {new}; remove it and re-create", path.display())]
    Pinned {
        path: PathBuf,
        old: String,
        mode: String,
        new: String,
    },
    #[error("branch {branch} is already checked out at {}{hint}", path.display())]
    BranchCheckedOut {
        branch: String,
        path: PathBuf,
        /// Non-empty only when that tree is prunable, so the refusal alone
        /// would leave no way out: one text when its directory is actually
        /// gone (`wt rm` cleans it up unaided), another when only its
        /// `.git` link is broken (the directory needs manual attention
        /// first; see `BrokenGitLink`).
        hint: &'static str,
    },
    #[error("{} exists but is not a worktree; remove it or pick another name", path.display())]
    NotAWorktree { path: PathBuf },
    #[error("--run has nowhere to run without --open")]
    RunWithoutOpen,
    #[error("could not seed {rel}: {source}")]
    Seed { rel: String, source: std::io::Error },
    #[error("cannot read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("exec failed in {}: {first} (output above; worktree rolled back)", path.display())]
    ExecStrict { path: PathBuf, first: String },
    #[error("{count} exec step(s) failed; the worktree is at {}", path.display())]
    ExecPartial { count: usize, path: PathBuf },
    #[error("{count} post-create step(s) failed; the worktree is at {}", path.display())]
    PostCreate { count: usize, path: PathBuf },
    #[error("give a name/branch, or --stale / --merged")]
    RmWhat,
    #[error("refusing to remove the main worktree")]
    MainWorktree,
    #[error(
        "{}: its .git link is broken, so git cannot remove it; delete the directory (or run `git worktree repair`) and run `wt rm` again",
        path.display()
    )]
    BrokenGitLink { path: PathBuf },
    #[error("{}: no readable wt metadata. Re-run with --force to remove it under the default policy.", path.display())]
    NoMetadata { path: PathBuf },
    #[error("{}: {reasons} (use --force to override)", path.display())]
    NotClean { path: PathBuf, reasons: String },
    #[error("could not purge {}: {source}", path.display())]
    Purge {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove the sidecar {}: {source}", path.display())]
    SidecarLeft {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not remove {}: {detail}", path.display())]
    RemoveFailed { path: PathBuf, detail: String },
    #[error("could not list branches merged into {base}: {detail}")]
    MergedList { base: String, detail: String },
    #[error("refusing to use '{0}' from the origin URL")]
    UnsafeRemotePart(String),
    /// An external CLI (az, gh) failed or answered wrongly.
    #[error("{0}")]
    Cli(String),
    #[error("{failed} of {total} worktrees could not be removed")]
    SweepFailed { failed: usize, total: usize },
    #[error(
        "naming fell back to the mechanical slug ({typ}/{slug}): {reason}. That name would be pushed and linked. Re-run with --no-llm to accept it, or pass --slug."
    )]
    NamingFellBack {
        typ: String,
        slug: String,
        reason: String,
    },
    #[error("no remote branch {0}")]
    NoRemoteBranch(String),
    #[error("hook payload was not JSON")]
    HookPayloadNotJson,
    #[error("hook payload was a JSON {kind}, expected an object")]
    HookPayloadNotObject { kind: String },
    #[error("hook-remove: payload carried no worktreePath")]
    HookRemoveNoTarget,
    #[error("hook-remove: no worktree at {}", python_repr(target))]
    HookRemoveNoMatch { target: String },
}

/// A minimal `repr()` for a plain path string: single-quoted, backslashes
/// and single quotes escaped as Python's `repr` escapes them. Python falls
/// back to double quotes when a string holds a `'` but no `"`; a hook
/// payload's path is never expected to, so that fallback is not ported.
pub fn python_repr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            _ => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_remove_no_match_renders_a_python_style_repr() {
        let err = AppError::HookRemoveNoMatch {
            target: "feat/hooked".to_owned(),
        };
        assert_eq!(err.to_string(), "hook-remove: no worktree at 'feat/hooked'");
    }

    #[test]
    fn python_repr_escapes_backslashes_and_quotes() {
        assert_eq!(python_repr(r"C:\repo\x"), r"'C:\\repo\\x'");
        assert_eq!(python_repr("it's"), r"'it\'s'");
    }
}
