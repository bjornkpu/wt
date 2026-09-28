// Marks this file as test code, so clippy.toml allows unwrap in its helpers too.
#![cfg(test)]
// Shared integration-test harness: a throwaway repo with a real bare origin,
// isolated from the developer's own git config and `~/.config/wt`. Later
// milestones reuse this (rename `make_tree` to call `wt new` once it exists;
// for now, M2 has no create verb, so trees are made with plain `git worktree
// add` plus a hand-written sidecar).
#![allow(dead_code)] // not every test file uses every helper

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub struct Env {
    pub root: PathBuf,
}

impl Env {
    /// A fresh repo with a bare origin, main branch, one commit, and
    /// `origin/HEAD` set - so `default_branch`-style lookups have something
    /// real to find, same as the Python test fixture.
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("wt-it-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // `TEMP`/`TMP` can be an 8.3 short form (GitHub's windows-latest
        // runner sets it that way for `runneradmin`); git always reports
        // paths in their long canonical form, so a test built from the short
        // root would never match wt's own output. Canonicalizing - then
        // stripping the `\\?\` verbatim prefix `canonicalize` adds - puts
        // every test on the same long form git uses.
        #[cfg(windows)]
        let root = {
            let canon = std::fs::canonicalize(&root).unwrap();
            let s = canon.to_str().unwrap();
            PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(s))
        };
        std::fs::create_dir_all(root.join("wt-home")).unwrap();
        std::fs::write(root.join("gitconfig"), "").unwrap();
        std::fs::write(root.join("wt-home/config.toml"), "[naming]\nllm = false\n").unwrap();
        std::fs::create_dir_all(root.join("repo")).unwrap();

        let env = Self { root };
        let origin = env.root.join("origin.git");
        env.run_git(
            &env.root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );
        env.git(&["init", "-q", "-b", "main", "."]);
        env.git(&["config", "user.email", "t@t"]);
        env.git(&["config", "user.name", "t"]);
        std::fs::write(env.repo().join("f.txt"), "x").unwrap();
        // .env is what `copy` seeds in the new tests; ignored, so a seeded
        // tree still reads as clean.
        std::fs::write(env.repo().join(".gitignore"), ".env\n").unwrap();
        env.git(&["add", "f.txt", ".gitignore"]);
        env.git(&["commit", "-qm", "init"]);
        env.git(&["remote", "add", "origin", origin.to_str().unwrap()]);
        env.git(&["push", "-q", "-u", "origin", "main"]);
        env.git(&["remote", "set-head", "origin", "-a"]);
        env
    }

    pub fn repo(&self) -> PathBuf {
        self.root.join("repo")
    }

    /// Where `wt_root_of`'s default (`../{repo}.worktrees`) puts new trees,
    /// sibling to the repo checkout.
    pub fn worktrees_root(&self) -> PathBuf {
        self.root.join("repo.worktrees")
    }

    /// A `program` invocation isolated from the developer's own git config
    /// and pointed at this env's `WT_HOME`. `pub`: the init smoke tests use
    /// it to spawn `pwsh`/`zsh` with the same isolation as `wt` itself, so
    /// their child `wt` calls stay just as sandboxed.
    pub fn command(&self, program: &str, cwd: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "BK")
            .env("GIT_AUTHOR_EMAIL", "bk@example.com")
            .env("GIT_COMMITTER_NAME", "BK")
            .env("GIT_COMMITTER_EMAIL", "bk@example.com")
            .env("WT_HOME", self.root.join("wt-home"))
            .env("PATH", self.hidden_path(&["herdr"]));
        cmd
    }

    fn run_git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.command("git", cwd).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    pub fn git(&self, args: &[&str]) -> String {
        self.run_git(&self.repo(), args)
    }

    /// `git` run in a linked worktree instead of the main checkout.
    pub fn git_at(&self, cwd: &Path, args: &[&str]) -> String {
        self.run_git(cwd, args)
    }

    /// Runs the built `wt` binary in the repo checkout, returning its raw
    /// output (both streams captured, exit code included) so a test can
    /// assert on stdout, stderr and failure alike.
    pub fn wt(&self, args: &[&str]) -> Output {
        self.wt_at(&self.repo(), args)
    }

    /// Like `wt`, but run from an arbitrary cwd - a subdirectory of the
    /// repo, or a linked worktree - instead of the repo root.
    pub fn wt_at(&self, cwd: &Path, args: &[&str]) -> Output {
        self.command(env!("CARGO_BIN_EXE_wt"), cwd)
            .args(args)
            .output()
            .unwrap()
    }

    /// Like `wt_at`, but with `PATH` replaced by `path_override` - the
    /// naming test that pins PATH to a directory with only `git` in it, so
    /// `claude` cannot be found.
    pub fn wt_at_with_path(&self, cwd: &Path, args: &[&str], path_override: &str) -> Output {
        self.command(env!("CARGO_BIN_EXE_wt"), cwd)
            .env("PATH", path_override)
            .args(args)
            .output()
            .unwrap()
    }

    /// Like `wt`, but pipes `stdin` to the process instead of leaving it
    /// inherited - what `hook-create`/`hook-remove` read their JSON payload
    /// from.
    pub fn wt_stdin(&self, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .command(env!("CARGO_BIN_EXE_wt"), &self.repo())
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Replaces the global config with `[naming] llm = false` plus `extra`.
    pub fn set_config(&self, extra: &str) {
        std::fs::write(
            self.root.join("wt-home/config.toml"),
            format!("[naming]\nllm = false\n{extra}"),
        )
        .unwrap();
    }

    /// Registered worktree paths other than the main checkout.
    pub fn trees(&self) -> Vec<String> {
        self.git(&["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .skip(1)
            .map(str::to_owned)
            .collect()
    }

    pub fn branches(&self) -> Vec<String> {
        self.git(&["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// PATH with every directory holding one of `hide` removed - except
    /// that, on unix, a directory that also holds `git`/`sh`/`bash` survives
    /// as a per-`Env` "keep" dir of symlinks to just those tools (cleaned up
    /// with the rest of `root`), in the original's place. Needed because
    /// GitHub's ubuntu runner keeps `gh` (and `az`) in `/usr/bin`, right next
    /// to `git` and `sh`: dropping that whole directory took git down with
    /// it. Windows keeps the simple drop - git lives in its own directory
    /// there.
    // On Windows, `self` (used to scope the unix-only keep dir below) goes
    // unused - git lives in its own directory there, so a plain drop is
    // enough.
    #[allow(clippy::unused_self)]
    pub fn hidden_path(&self, hide: &[&str]) -> std::ffi::OsString {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut kept: Vec<PathBuf> = Vec::new();
        for dir in std::env::split_paths(&path) {
            if hide.iter().any(|t| holds(&dir, t)) {
                #[cfg(unix)]
                {
                    let survivors: Vec<&str> = KEEP_TOOLS
                        .iter()
                        .copied()
                        .filter(|t| holds(&dir, t))
                        .collect();
                    if !survivors.is_empty() {
                        // `kept.len()` as a name, not a counter: no `+= 1` to trip
                        // `arithmetic_side_effects`.
                        let keep_dir = self.root.join("path-keep").join(kept.len().to_string());
                        std::fs::create_dir_all(&keep_dir).unwrap();
                        for tool in survivors {
                            std::os::unix::fs::symlink(dir.join(tool), keep_dir.join(tool))
                                .unwrap();
                        }
                        kept.push(keep_dir);
                    }
                }
            } else {
                kept.push(dir);
            }
        }
        assert!(
            kept.iter().any(|dir| holds(dir, "git")),
            "filtering {hide:?} out of PATH also lost git"
        );
        std::env::join_paths(kept).unwrap()
    }

    /// Sidecar file names under the shared `.git/wt`.
    pub fn sidecars(&self) -> Vec<String> {
        std::fs::read_dir(self.repo().join(".git").join("wt"))
            .map(|dir| {
                dir.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn stdout(out: &Output) -> String {
        String::from_utf8(out.stdout.clone()).unwrap()
    }

    pub fn stderr(out: &Output) -> String {
        String::from_utf8(out.stderr.clone()).unwrap()
    }

    /// A worktree at `worktrees_root()/leaf`, checked out on a new branch,
    /// with a sidecar recording `mode` - standing in for `wt new` (M3).
    pub fn make_tree(&self, leaf: &str, mode: &str) -> PathBuf {
        let path = self.worktrees_root().join(leaf);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.git(&[
            "worktree",
            "add",
            "-b",
            leaf,
            path.to_str().unwrap(),
            "main",
        ]);
        self.write_sidecar(&path, mode, Some(leaf));
        path
    }

    /// A detached worktree (no branch), the way `wt pr`/`wt branch --detach`
    /// leave one.
    pub fn make_detached_tree(&self, leaf: &str, mode: &str) -> PathBuf {
        let path = self.worktrees_root().join(leaf);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        self.git(&[
            "worktree",
            "add",
            "--detach",
            path.to_str().unwrap(),
            "main",
        ]);
        self.write_sidecar(&path, mode, None);
        path
    }

    fn write_sidecar(&self, path: &Path, mode: &str, branch: Option<&str>) {
        let meta_dir = self.repo().join(".git").join("wt");
        std::fs::create_dir_all(&meta_dir).unwrap();
        let branch_json = branch.map_or_else(|| "null".to_owned(), |b| format!("\"{b}\""));
        let name: String = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '_' })
            .collect();
        let file = meta_dir.join(format!("{name}-{}.json", std::process::id()));
        let path_json = serde_json::to_string(&path.display().to_string()).unwrap();
        let json = format!(
            "{{\"mode\":\"{mode}\",\"path\":{path_json},\"branch\":{branch_json},\"created\":\"2026-01-01T00:00:00+00:00\"}}"
        );
        std::fs::write(file, json).unwrap();
    }
}

/// The 8.3 short name form of `path` (e.g. `C:\...\REPO-L~1`) - the concrete
/// shape the review that flagged finding 1 reproduced the `common_dir` bug
/// from. Only meaningful on Windows, and only when NTFS short-name
/// generation is enabled for the volume (the default). Goes through a
/// one-line batch file (`%~s1`) rather than `cmd /c "<script>"`: cmd's own
/// quote-stripping on `/c` gets confused once the script itself contains
/// quotes, so a bare path argument (no embedded quoting at all) is the
/// robust way to hand it a path.
#[cfg(windows)]
pub fn short_path(path: &Path) -> PathBuf {
    let bat = std::env::temp_dir().join(format!("wt-shortpath-{}.cmd", std::process::id()));
    std::fs::write(&bat, "@echo %~s1\r\n").unwrap();
    let out = Command::new("cmd")
        .arg("/c")
        .arg(&bat)
        .arg(path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&bat);
    assert!(
        out.status.success(),
        "short_path({}): {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    PathBuf::from(String::from_utf8(out.stdout).unwrap().trim())
}

/// `name` in `dir` under any name `cli::which` would try: bare, plus every
/// PATHEXT extension on Windows.
fn holds(dir: &Path, name: &str) -> bool {
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    std::iter::once("")
        .chain(exts.split(';').filter(|e| !e.is_empty()))
        .any(|ext| dir.join(format!("{name}{ext}")).is_file())
}

/// Tools `Env::hidden_path` keeps a hidden directory's copy of, on unix: the
/// tests' own `git`, plus the shell git hooks and local pushes need.
#[cfg(unix)]
const KEEP_TOOLS: &[&str] = &["git", "sh", "bash"];

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
