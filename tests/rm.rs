// Test code throughout, so clippy.toml allows unwrap in the helpers too.
#![cfg(test)]

mod common;

use std::path::{Path, PathBuf};

use common::Env;

/// A tree made by `wt new`, the way the Python tests make theirs through
/// `create()`: seeded with `.env`, recorded in a sidecar.
fn new_tree(env: &Env, branch: &str) -> PathBuf {
    let out = env.wt(&["-q", "new", branch]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    PathBuf::from(Env::stdout(&out).trim_end())
}

fn seeded_env(name: &str) -> Env {
    let env = Env::new(name);
    env.set_config("[defaults]\ncopy = [\".env\"]\n");
    std::fs::write(env.repo().join(".env"), "SECRET=1").unwrap();
    env
}

fn commit_in(env: &Env, path: &Path) {
    std::fs::write(path.join("wip.txt"), "x").unwrap();
    env.git_at(path, &["add", "wip.txt"]);
    env.git_at(path, &["commit", "-qm", "wip"]);
}

fn sidecar_file(env: &Env) -> PathBuf {
    let names = env.sidecars();
    let [name] = names.as_slice() else {
        panic!("expected one sidecar: {names:?}");
    };
    env.repo().join(".git").join("wt").join(name)
}

/// A refusal: exit 1, the message on stderr, nothing on stdout, the tree
/// and its sidecar untouched.
fn refused(env: &Env, out: &std::process::Output, path: &Path) -> String {
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(out));
    assert_eq!(Env::stdout(out), "");
    assert!(path.is_dir(), "the tree must survive a refusal");
    assert_eq!(env.trees().len(), 1);
    Env::stderr(out)
}

// ------------------------------------------------------------- TestAcceptance

#[test]
fn test_new_ls_rm_round_trip() {
    let env = seeded_env("rm-acceptance");
    let path = new_tree(&env, "feat/acceptance-probe");
    assert!(path.join(".env").is_file());
    let ls = Env::stdout(&env.wt(&["ls"]));
    assert!(ls.contains("feat/acceptance-probe"), "{ls}");

    let out = env.wt(&["rm", "feat/acceptance-probe"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "", "rm prints nothing on stdout");
    assert_eq!(Env::stderr(&out), format!("removed: {}\n", path.display()));
    assert!(!path.exists());
    assert_eq!(env.sidecars(), Vec::<String>::new(), "sidecar cleaned up");
    assert_eq!(env.trees(), Vec::<String>::new(), "no stale registration");
    assert!(!env.branches().contains(&"feat/acceptance-probe".to_owned()));
}

#[test]
fn rm_without_a_name_says_what_it_wants() {
    let env = Env::new("rm-noname");
    let out = env.wt(&["rm"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        Env::stderr(&out),
        "wt: give a name/branch, or --stale / --merged\n"
    );
}

// ------------------------------------------------------------ TestDirtyReasons

#[test]
fn test_clean_tree_dry_run_changes_nothing() {
    let env = seeded_env("rm-dry");
    let path = new_tree(&env, "feat/preview");
    let out = env.wt(&["rm", "--dry-run", "feat/preview"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "");
    assert_eq!(
        Env::stderr(&out),
        format!(
            "would remove: {}\n  mode:   new\n  branch: feat/preview -> if_merged\n  purge:  .env\n",
            path.display()
        ),
        "a clean tree has no NOT CLEAN line"
    );
    assert!(path.join(".env").is_file());
    assert!(
        sidecar_file(&env).is_file(),
        "sidecar must survive a dry run"
    );
    assert_eq!(env.trees().len(), 1);
}

#[test]
fn test_refuses_dirty_tree() {
    let env = seeded_env("rm-dirty");
    let path = new_tree(&env, "feat/blocked");
    std::fs::write(path.join("untracked.txt"), "x").unwrap();
    let out = env.wt(&["rm", "feat/blocked"]);
    assert_eq!(
        refused(&env, &out, &path),
        format!(
            "wt: {}: uncommitted changes (use --force to override)\n",
            path.display()
        )
    );
    assert!(path.join(".env").is_file(), "nothing purged on a refusal");
}

#[test]
fn test_unpushed_commits() {
    let env = Env::new("rm-unpushed");
    let path = new_tree(&env, "feat/unpushed");
    commit_in(&env, &path);
    let out = env.wt(&["rm", "feat/unpushed"]);
    let stderr = refused(&env, &out, &path);
    assert!(
        stderr.contains(": unpushed commits (use --force"),
        "{stderr}"
    );
}

#[test]
fn test_a_stash_elsewhere_in_the_repo_does_not_block() {
    let env = Env::new("rm-stash");
    let path = new_tree(&env, "feat/notmine");
    std::fs::write(env.repo().join("f.txt"), "stash me").unwrap();
    env.git(&["stash", "-q"]);
    assert!(env.git_at(&path, &["stash", "list"]).contains("stash@"));
    let out = env.wt(&["rm", "feat/notmine"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}

#[test]
fn test_missing_tracking_ref_is_a_reason_not_silence() {
    let env = Env::new("rm-noref");
    let path = new_tree(&env, "feat/noref");
    env.git(&["update-ref", "-d", "refs/remotes/origin/main"]);
    let out = env.wt(&["rm", "feat/noref"]);
    let stderr = refused(&env, &out, &path);
    assert!(
        stderr.contains("could not compare against origin/main"),
        "{stderr}"
    );
}

#[test]
fn test_force_removes_dirty_tree() {
    let env = seeded_env("rm-force");
    let path = new_tree(&env, "feat/forced");
    std::fs::write(path.join("untracked.txt"), "x").unwrap();
    commit_in(&env, &path);
    let out = env.wt(&["-q", "rm", "--force", "feat/forced"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stderr(&out), "", "-q: nothing but failures");
    assert!(!path.exists());
    assert_eq!(env.sidecars(), Vec::<String>::new());
    assert!(
        env.branches().contains(&"feat/forced".to_owned()),
        "if_merged keeps the unmerged branch even under --force"
    );
}

// ---------------------------------------------------------- TestTeardownPolicy

#[test]
fn test_refuses_without_sidecar_and_f21_dry_run_previews_it() {
    let env = Env::new("rm-nometa");
    let path = new_tree(&env, "feat/nometa");
    std::fs::remove_file(sidecar_file(&env)).unwrap();

    let out = env.wt(&["rm", "feat/nometa"]);
    assert_eq!(
        refused(&env, &out, &path),
        format!(
            "wt: {}: no readable wt metadata. Re-run with --force to remove it under the default policy.\n",
            path.display()
        )
    );

    let out = env.wt(&["rm", "--dry-run", "feat/nometa"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(
        Env::stderr(&out),
        format!(
            "would remove: {}\n  mode:   (no metadata)\n  branch: feat/nometa -> if_merged\n  purge:  (nothing)\n",
            path.display()
        )
    );
    assert!(path.is_dir());

    let out = env.wt(&["rm", "--force", "feat/nometa"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}

/// A locked tree is a real removal failure: `git worktree remove --force`
/// refuses it every time. The purge ran first (that is its point), stayed
/// inside the tree, and the sidecar and branch outlive the failure.
#[test]
fn test_sidecar_survives_a_failed_removal_and_the_purge_stays_inside() {
    let env = seeded_env("rm-stuck");
    std::fs::create_dir_all(env.repo().join(".databricks")).unwrap();
    std::fs::write(env.repo().join(".databricks/cfg"), "x").unwrap();
    env.set_config("[defaults]\ncopy = [\".env\", \".databricks\"]\n");
    let path = new_tree(&env, "feat/stuck");
    assert!(path.join(".databricks/cfg").is_file());
    std::fs::write(path.join("keep.txt"), "mine").unwrap();
    let canary = env.root.join("canary.txt");
    std::fs::write(&canary, "do not delete").unwrap();
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), "do not delete").unwrap();
    let linked = symlink_dir(&outside, &path.join("link")).is_ok();

    let file = sidecar_file(&env);
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    meta["copied"] = serde_json::json!([
        ".env",
        ".databricks",
        ".missing",
        "../../../canary.txt",
        "link/secret"
    ]);
    std::fs::write(&file, meta.to_string()).unwrap();
    env.git(&["worktree", "lock", path.to_str().unwrap()]);

    let out = env.wt(&["rm", "--force", "feat/stuck"]);
    let stderr = refused(&env, &out, &path);
    assert!(
        stderr.starts_with(&format!("wt: could not remove {}: ", path.display())),
        "{stderr}"
    );
    assert!(stderr.contains("locked"), "git's own reason: {stderr}");
    assert!(!path.join(".env").exists(), "the copied secret is purged");
    assert!(!path.join(".databricks").exists(), "a copied directory too");
    assert!(path.join("keep.txt").is_file(), "the rest is left alone");
    assert!(canary.is_file(), "purge escaped the worktree");
    if linked {
        assert!(
            outside.join("secret").is_file(),
            "purge followed a symlink out"
        );
    }
    assert!(
        file.is_file(),
        "the policy record outlives the failed removal"
    );
    assert!(env.branches().contains(&"feat/stuck".to_owned()));
}

#[cfg(windows)]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, dst)
}

#[cfg(not(windows))]
fn symlink_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

/// wt run from inside the tree it removes: its own cwd must not be what
/// keeps the directory alive (Windows will not delete a process's cwd).
#[test]
fn rm_from_inside_the_tree_removes_it_entirely() {
    let env = Env::new("rm-inside");
    let path = new_tree(&env, "feat/inside");
    let out = env.wt_at(&path, &["rm", "feat/inside"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new());
}

/// What Windows does when another process sits in the tree: git deletes the
/// files, deregisters the worktree, cannot unlink the directory and exits
/// non-zero. Judged by registration, that removal happened.
#[cfg(windows)]
#[test]
fn test_a_deregistered_tree_finishes_the_teardown() {
    let env = Env::new("rm-held");
    let path = new_tree(&env, "feat/emptied");
    let mut holder = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", "Start-Sleep -Seconds 60"])
        .current_dir(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let out = env.wt(&["rm", "feat/emptied"]);
    let _ = holder.kill();
    let _ = holder.wait();
    let stderr = Env::stderr(&out);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(env.trees(), Vec::<String>::new());
    assert_eq!(
        env.sidecars(),
        Vec::<String>::new(),
        "the sidecar must not outlive the teardown"
    );
    assert!(!env.branches().contains(&"feat/emptied".to_owned()));
    assert!(
        stderr.contains(&format!("removed: {}", path.display())),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "wt: the empty directory is still held, remove it later: {}",
            path.display()
        )),
        "{stderr}"
    );
}

// ------------------------------------------------------- TestDeleteBranchPolicy

fn remove_with_policy(name: &str, policy: &str, commit: bool) -> (Env, std::process::Output) {
    let env = Env::new(name);
    env.set_config(&format!("[teardown]\ndelete_branch = \"{policy}\"\n"));
    let path = new_tree(&env, "feat/x");
    if commit {
        commit_in(&env, &path);
    }
    let out = env.wt(&["rm", "--force", "feat/x"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
    (env, out)
}

#[test]
fn test_never_keeps_the_branch() {
    let (env, _) = remove_with_policy("rm-never", "never", false);
    assert!(env.branches().contains(&"feat/x".to_owned()));
}

#[test]
fn test_always_drops_an_unmerged_branch() {
    let (env, _) = remove_with_policy("rm-always", "always", true);
    assert!(!env.branches().contains(&"feat/x".to_owned()));
}

#[test]
fn test_if_merged_keeps_an_unmerged_branch() {
    let (env, out) = remove_with_policy("rm-unmerged", "if_merged", true);
    assert!(env.branches().contains(&"feat/x".to_owned()));
    assert!(
        Env::stderr(&out).contains("  kept branch feat/x: error: "),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn test_if_merged_drops_a_merged_branch() {
    let (env, _) = remove_with_policy("rm-merged", "if_merged", false);
    assert!(!env.branches().contains(&"feat/x".to_owned()));
}

// ---------------------------------------------------- TestMainWorktreeProtected

#[test]
fn test_refuses_to_remove_main() {
    let env = Env::new("rm-main");
    let out = env.wt(&["rm", "--force", "main"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(Env::stderr(&out), "wt: no worktree matching 'main'\n");
    assert!(env.repo().join(".git").is_dir());
}

// ------------------------------------------------------------ TestModeScopedRoot

/// A tree the Python tool made: `git worktree add` at a mode-scoped root, and
/// the sidecar exactly as `json.dumps(meta, indent=2)` + `write_text` leave
/// it (CRLF on Windows, native backslash path, Python's key order, a
/// `created` with microseconds, no trailing newline).
fn python_tree(env: &Env, path: &Path, leaf: &str, mode: &str, branch: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let p = path.to_str().unwrap();
    let arm = branch.map_or_else(|| vec!["--detach"], |b| vec!["-b", b]);
    let args = [&["worktree", "add", "-q"][..], &arm, &[p, "origin/main"]].concat();
    env.git(&args);
    std::fs::write(path.join(".env"), "SECRET=1").unwrap();
    let nl = if cfg!(windows) { "\r\n" } else { "\n" };
    let branch_json = branch.map_or_else(|| "null".to_owned(), |b| format!("\"{b}\""));
    let lines = [
        "{".to_owned(),
        format!("  \"mode\": \"{mode}\","),
        format!("  \"path\": {},", serde_json::to_string(p).unwrap()),
        format!("  \"branch\": {branch_json},"),
        "  \"base\": \"origin/main\",".to_owned(),
        format!("  \"detached\": {},", branch.is_none()),
        "  \"copied\": [".to_owned(),
        "    \".env\"".to_owned(),
        "  ],".to_owned(),
        "  \"created\": \"2026-09-01T08:15:30.123456+00:00\"".to_owned(),
        "}".to_owned(),
    ];
    let dir = env.repo().join(".git").join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    let name = leaf.replace('/', "%2F");
    std::fs::write(dir.join(format!("{name}.json")), lines.join(nl)).unwrap();
}

#[test]
fn test_sidecar_found_when_a_mode_overrides_root() {
    let env = Env::new("rm-scoped");
    env.set_config(
        "[mode.pr]\nroot = \"../{repo}.reviews\"\n[teardown.mode.pr]\nrequire_clean = false\ndelete_branch = \"never\"\n",
    );
    let path = env.root.join("repo.reviews").join("review").join("pr-4521");
    python_tree(&env, &path, "review/pr-4521", "pr", None);

    let out = env.wt(&["rm", "--dry-run", "review/pr-4521"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(
        Env::stderr(&out),
        format!(
            "would remove: {}\n  mode:   pr\n  branch: (detached) -> never\n  purge:  .env\n",
            path.display()
        ),
        "the pr policy, found through the pr root"
    );
}

/// Drop-in: a Python-written sidecar is found by path and its policy
/// honoured. `delete_branch = "never"` for its mode keeps a merged branch
/// the default `if_merged` would have dropped.
#[test]
fn a_sidecar_the_python_tool_wrote_is_torn_down_under_its_policy() {
    let env = Env::new("rm-python");
    env.set_config("[teardown.mode.branch]\ndelete_branch = \"never\"\n");
    let path = env.worktrees_root().join("feat").join("py-made");
    python_tree(&env, &path, "feat/py-made", "branch", Some("feat/py-made"));

    let out = env.wt(&["rm", "feat/py-made"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
    assert_eq!(env.sidecars(), Vec::<String>::new());
    assert_eq!(env.trees(), Vec::<String>::new());
    assert!(
        env.branches().contains(&"feat/py-made".to_owned()),
        "the sidecar's mode decided the policy"
    );
}

// ---------------------------------------------------------- TestConfiguredRemote

#[test]
fn test_the_dirty_check_compares_against_the_configured_remote() {
    let env = Env::new("rm-upstream");
    env.git(&["remote", "rename", "origin", "upstream"]);
    env.set_config("[defaults]\nremote = \"upstream\"\nbase = \"{remote}/{default_branch}\"\n");
    let path = new_tree(&env, "feat/onupstream");
    env.git(&["update-ref", "-d", "refs/remotes/upstream/main"]);
    let out = env.wt(&["rm", "feat/onupstream"]);
    let stderr = refused(&env, &out, &path);
    assert!(
        stderr.contains("could not compare against upstream/main"),
        "{stderr}"
    );
}

/// The sidecar held open without delete sharing: readable, not removable.
/// The teardown still finishes, then the run fails naming the sidecar.
#[cfg(windows)]
#[test]
fn a_sidecar_that_cannot_be_removed_fails_the_run_after_the_teardown() {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 0x1;
    let env = Env::new("rm-sidecar-held");
    let path = new_tree(&env, "feat/held");
    let file = sidecar_file(&env);
    let holder = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&file)
        .unwrap();
    let out = env.wt(&["rm", "feat/held"]);
    drop(holder);
    let stderr = Env::stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(!path.exists(), "{stderr}");
    assert_eq!(env.trees(), Vec::<String>::new());
    assert!(!env.branches().contains(&"feat/held".to_owned()));
    assert!(
        stderr.contains(&format!("removed: {}", path.display())),
        "{stderr}"
    );
    assert!(
        stderr.contains(&format!(
            "wt: could not remove the sidecar {}: ",
            file.display()
        )),
        "{stderr}"
    );
}

#[test]
fn rm_judges_a_tree_against_the_default_branch_its_mode_created_it_from() {
    let env = Env::new("rm-mode-default-branch");
    // origin/develop is one commit past main; the remote cannot be asked
    // which branch is its default, so each mode's fallback decides.
    env.git(&["checkout", "-q", "-b", "develop"]);
    std::fs::write(env.repo().join("d.txt"), "d").unwrap();
    env.git(&["add", "d.txt"]);
    env.git(&["commit", "-qm", "develop"]);
    env.git(&["push", "-q", "origin", "develop"]);
    env.git(&["checkout", "-q", "main"]);
    env.git(&["remote", "set-head", "origin", "-d"]);
    let gone = env.root.join("no-such-origin.git");
    env.git(&["remote", "set-url", "origin", gone.to_str().unwrap()]);
    env.set_config("[defaults]\nfetch = false\n[mode.new]\ndefault_branch = \"develop\"\n");
    // No upstream for the new branch: the base is what it is counted against.
    std::fs::write(
        env.root.join("gitconfig"),
        "[branch]\n\tautoSetupMerge = false\n",
    )
    .unwrap();

    let _path = new_tree(&env, "feat/on-develop");
    let out = env.wt(&["rm", "feat/on-develop"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new());
}
