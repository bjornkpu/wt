use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use crate::error::AppError;
use crate::io::cli;

/// Network calls (fetch, push); most git calls are local and finish well
/// inside this.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);
/// A ref lookup, not a fetch: it must not sit on the fetch timeout.
const REF_TIMEOUT: Duration = Duration::from_secs(20);

/// One completed git invocation: the exit status plus its output, decoded
/// as UTF-8 (lossily: git's own encoding is UTF-8, but a stray byte must
/// not crash wt).
#[derive(Debug, Clone)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs git with a null stdin (so a credential prompt fails instead of
/// hanging on a pipe nobody reads) and a timeout, returning the raw result
/// even on a non-zero exit. Use `run` when a non-zero exit should become an
/// error instead.
pub fn try_run(args: &[&str], cwd: &Path) -> Result<Output, AppError> {
    try_run_timeout(args, cwd, GIT_TIMEOUT)
}

fn try_run_timeout(args: &[&str], cwd: &Path, timeout: Duration) -> Result<Output, AppError> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| AppError::GitSpawn { source })?;

    // Read both pipes on their own threads: wait_timeout does not drain
    // them, and a child that writes more than one pipe buffer before
    // exiting would otherwise deadlock against a parent blocked in wait.
    let stdout = cli::capture(child.stdout.take());
    let stderr = cli::capture(child.stderr.take());

    let wait_result = child
        .wait_timeout(timeout)
        .map_err(|source| AppError::GitSpawn { source })?;
    let Some(status) = wait_result else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(AppError::GitTimeout {
            args: args.join(" "),
            timeout: timeout.as_secs(),
        });
    };
    // git has exited, but a helper it started (remote-https, a credential
    // manager, ssh ControlPersist) may still hold the pipes. A held pipe
    // yields nothing: its reader only sends at EOF.
    let draining = Instant::now();
    let [stdout, stderr] = [stdout, stderr].map(|rx| {
        rx.recv_timeout(cli::DRAIN_GRACE.saturating_sub(draining.elapsed()))
            .ok()
    });
    let [stdout, stderr] = match [stdout, stderr] {
        [Some(stdout), Some(stderr)] => [stdout, stderr],
        _ if !status.success() => {
            return Err(AppError::GitHeld {
                args: args.join(" "),
            });
        }
        held => {
            eprintln!(
                "wt: git {} exited but left processes holding its output; continuing",
                args.join(" ")
            );
            held.map(Option::unwrap_or_default)
        }
    };
    Ok(Output {
        success: status.success(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

/// Runs git and dies on a non-zero exit, the way the Python tool's `git()`
/// does: `git {args}\n{stderr or stdout}`. Returns stripped stdout.
pub fn run(args: &[&str], cwd: &Path) -> Result<String, AppError> {
    let out = try_run(args, cwd)?;
    if !out.success {
        let text = if out.stderr.trim().is_empty() {
            &out.stdout
        } else {
            &out.stderr
        };
        return Err(AppError::Git {
            args: args.join(" "),
            message: text.trim().to_owned(),
        });
    }
    Ok(out.stdout.trim().to_owned())
}

/// The shared `.git` directory, in exactly the form `git worktree list
/// --porcelain` itself reports paths in (git's own canonical form:
/// forward-slashed even on Windows). `--path-format=absolute` (git >= 2.31)
/// does this in one call; anything else - joining a relative
/// `--git-common-dir` onto `cwd`, or `fs::canonicalize` - takes on `cwd`'s
/// own shape instead (an 8.3 short name, a `subst`ed drive, a
/// `..`-collapsed relative path), which then compares unequal, component by
/// component, to the porcelain form for the exact same directory. That
/// mismatch is what silently dropped `(main)` from `ls`, mis-resolved
/// leaves, and let `complete` offer the main tree when wt ran from such a
/// cwd, or from inside a linked worktree.
pub fn common_dir(cwd: &Path) -> Result<std::path::PathBuf, AppError> {
    let raw = run(
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd,
    )?;
    Ok(std::path::PathBuf::from(raw))
}

/// The remote's default branch, or `None` when neither the local
/// `<remote>/HEAD` nor the remote itself says. Guessing "main" in a
/// master/develop repo produces a base ref that never resolves, and blames
/// git for it.
#[must_use]
pub fn remote_default_branch(remote: &str, root: &Path) -> Option<String> {
    let quiet_ref = format!("refs/remotes/{remote}/HEAD");
    if let Ok(out) = try_run(&["symbolic-ref", "--quiet", &quiet_ref], root)
        && out.success
        && let Some(branch) =
            crate::domain::parse::default_branch_from_symbolic_ref(remote, &out.stdout)
    {
        return Some(branch);
    }
    if let Ok(out) = try_run_timeout(
        &["ls-remote", "--symref", remote, "HEAD"],
        root,
        REF_TIMEOUT,
    ) && out.success
        && let Some(branch) = crate::domain::parse::default_branch_from_ls_remote(&out.stdout)
    {
        return Some(branch);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_branch_is_none_when_the_remote_cannot_be_read() {
        // No git repository at all here, so both the symbolic-ref and the
        // ls-remote lookups fail.
        let dir = std::env::temp_dir();
        assert_eq!(remote_default_branch("origin", &dir), None);
    }

    #[test]
    fn a_helper_left_holding_the_pipes_does_not_hang_git() {
        // The alias's shell backgrounds a sleeper that inherits git's
        // stdout, the way a credential helper or ssh ControlPersist can.
        let started = std::time::Instant::now();
        let out = try_run(
            &["-c", "alias.hold=!sleep 30 & echo hi", "hold"],
            &std::env::temp_dir(),
        );
        assert!(started.elapsed() < Duration::from_secs(15), "{out:?}");
        assert!(matches!(out, Ok(Output { success: true, .. })), "{out:?}");
    }

    #[test]
    fn a_failed_git_with_held_pipes_is_an_error() {
        let out = try_run(
            &["-c", "alias.hold=!sleep 30 & exit 3", "hold"],
            &std::env::temp_dir(),
        );
        assert!(matches!(out, Err(AppError::GitHeld { .. })), "{out:?}");
    }
}
