// Test code throughout, so clippy.toml allows unwrap in the helpers too.
#![cfg(test)]

mod common;

use std::path::{Path, PathBuf};

use common::Env;

/// A tree made by `wt new`, the way the Python tests make theirs.
fn new_tree(env: &Env, branch: &str) -> PathBuf {
    let out = env.wt(&["-q", "new", branch]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    PathBuf::from(Env::stdout(&out).trim_end())
}

fn commit_in(env: &Env, path: &Path) {
    std::fs::write(path.join("wip.txt"), "x").unwrap();
    env.git_at(path, &["add", "wip.txt"]);
    env.git_at(path, &["commit", "-qm", "wip"]);
}

/// Fast-forwards main (and its origin) onto `branch`'s tip, the way a
/// landed PR leaves the default branch.
fn merge_into_main(env: &Env, branch: &str) {
    env.git(&["merge", "-q", "--ff-only", branch]);
    env.git(&["push", "-q", "origin", "main"]);
}

/// Stands in for a tree whose directory was deleted by hand: it is still
/// registered with git (`prunable`), but nothing is left on disk.
fn delete_by_hand(path: &Path) {
    std::fs::remove_dir_all(path).unwrap();
}

/// Stands in for the other prunable shape the reviewer reproduced: only the
/// tree's own `.git` pointer file was deleted, leaving every other file on
/// disk. git reports this the same way it reports a fully deleted
/// directory: `prunable gitdir file points to non-existent location`.
fn delete_only_dot_git(path: &Path) {
    std::fs::remove_file(path.join(".git")).unwrap();
}

fn seeded_env(name: &str) -> Env {
    let env = Env::new(name);
    env.set_config("[defaults]\ncopy = [\".env\"]\n");
    std::fs::write(env.repo().join(".env"), "SECRET=1").unwrap();
    env
}

fn find_sidecar(env: &Env, path: &Path) -> PathBuf {
    let dir = env.repo().join(".git").join("wt");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let file = entry.unwrap().path();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        if v["path"].as_str() == Some(path.display().to_string().as_str()) {
            return file;
        }
    }
    panic!("no sidecar recorded for {}", path.display());
}

/// Ages the sidecar `new_tree` wrote so a `--stale` sweep judges it as
/// past its mode's `ttl_days`.
fn age_sidecar(env: &Env, path: &Path, days: i64) {
    let file = find_sidecar(env, path);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let now = jiff::Timestamp::now().as_second();
    let secs = now.checked_sub(days.checked_mul(86_400).unwrap()).unwrap();
    v["created"] = serde_json::json!(jiff::Timestamp::from_second(secs).unwrap().to_string());
    std::fs::write(&file, v.to_string()).unwrap();
}

#[test]
fn ls_shows_a_hand_deleted_tree_as_missing() {
    let env = Env::new("prunable-ls");
    let path = new_tree(&env, "feat/vanished");
    delete_by_hand(&path);

    let out = env.wt(&["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stdout = Env::stdout(&out);
    let line = stdout
        .lines()
        .find(|l| l.starts_with("feat/vanished"))
        .unwrap_or_else(|| panic!("no ls row for the vanished tree: {stdout}"));
    assert!(line.ends_with("missing"), "{line}");
}

#[test]
fn rm_cleans_up_a_hand_deleted_tree_without_force_and_drops_its_merged_branch() {
    let env = Env::new("prunable-rm");
    let path = new_tree(&env, "feat/gone");
    delete_by_hand(&path);

    // No --force: a prunable tree with a sidecar needs none.
    let out = env.wt(&["rm", "feat/gone"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new(), "registration cleaned up");
    assert_eq!(env.sidecars(), Vec::<String>::new(), "sidecar cleaned up");
    assert!(
        !env.branches().contains(&"feat/gone".to_owned()),
        "an unmodified branch is merged; if_merged's -d drops it"
    );
}

#[test]
fn rm_keeps_the_branch_of_a_hand_deleted_tree_when_it_has_unmerged_commits() {
    let env = Env::new("prunable-rm-unmerged");
    let path = new_tree(&env, "feat/unmerged");
    commit_in(&env, &path);
    delete_by_hand(&path);

    let out = env.wt(&["rm", "feat/unmerged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new());
    assert!(
        env.branches().contains(&"feat/unmerged".to_owned()),
        "if_merged's -d must refuse an unmerged branch, not lose the work"
    );
    assert!(
        Env::stderr(&out).contains("kept branch feat/unmerged: "),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn new_before_cleanup_refuses_with_a_hint_to_rm() {
    let env = Env::new("prunable-new-refused");
    let path = new_tree(&env, "feat/reborn");
    delete_by_hand(&path);

    let out = env.wt(&["new", "feat/reborn"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(Env::stdout(&out), "");
    let stderr = Env::stderr(&out);
    assert!(
        stderr.contains("branch feat/reborn is already checked out at"),
        "{stderr}"
    );
    assert!(
        stderr.contains("(its directory is gone; `wt rm <name>` cleans it up)"),
        "{stderr}"
    );
    assert_eq!(
        env.trees().len(),
        1,
        "still just the one (prunable) registration"
    );
}

#[test]
fn new_over_a_present_prunable_tree_hints_at_repair_not_rm() {
    let env = Env::new("prunable-new-broken-hint");
    let path = new_tree(&env, "feat/brokenhint");
    delete_only_dot_git(&path);

    let out = env.wt(&["new", "feat/brokenhint"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(Env::stdout(&out), "");
    let stderr = Env::stderr(&out);
    assert!(
        stderr.contains("branch feat/brokenhint is already checked out at"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "(its .git link is broken; delete the directory or run `git worktree repair`)"
        ),
        "{stderr}"
    );
    assert_eq!(env.trees().len(), 1);
}

/// The reviewer's CRITICAL reproduction: `prunable` does not mean the
/// directory is gone. Deleting only `<tree>/.git` leaves the seeded `.env`
/// and every other file in place. A dirty probe there is not merely
/// unreliable, it can be actively wrong (see the in-repo-root test below),
/// so `wt rm` refuses this shape outright, before any probe or purge,
/// rather than trusting a probe to catch it.
#[test]
fn rm_refuses_a_prunable_tree_whose_directory_is_still_present() {
    let env = seeded_env("prunable-present-leak");
    let path = new_tree(&env, "feat/leak");
    assert!(path.join(".env").is_file());
    delete_only_dot_git(&path);

    let out = env.wt(&["rm", "feat/leak"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "");
    let stderr = Env::stderr(&out);
    assert!(stderr.contains("its .git link is broken"), "{stderr}");

    assert!(path.is_dir(), "the tree must survive a refusal");
    assert!(
        path.join(".env").is_file(),
        "nothing purged from a tree that is still on disk"
    );
    assert!(
        path.join("f.txt").is_file(),
        "the rest of the tree survives too"
    );
    assert_eq!(env.trees().len(), 1, "still registered (prunable)");
    assert_eq!(env.sidecars().len(), 1, "sidecar untouched by the refusal");
}

/// Same shape, but `--force`: git itself refuses to remove a tree whose
/// `.git` link is broken even when forced (verified against real git), so
/// the old "probe, then purge before the removal fails anyway" path would
/// still destroy `.env` for nothing. Refused outright regardless of force.
#[test]
fn rm_refuses_a_present_prunable_tree_even_with_force() {
    let env = seeded_env("prunable-present-force");
    let path = new_tree(&env, "feat/forced-leak");
    delete_only_dot_git(&path);

    let out = env.wt(&["rm", "--force", "feat/forced-leak"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "");
    assert!(
        Env::stderr(&out).contains("its .git link is broken"),
        "{}",
        Env::stderr(&out)
    );
    assert!(
        path.join(".env").is_file(),
        "nothing purged, even with --force"
    );
    assert!(path.join("f.txt").is_file());
    assert_eq!(env.trees().len(), 1, "still registered (prunable)");
    assert_eq!(env.sidecars().len(), 1);
}

/// The reviewer's other CRITICAL reproduction: with an in-repo, gitignored
/// root (`root = ".worktrees"`), a dirty probe run with the broken tree as
/// cwd does not just fail to help - with the old fix it would walk *up*
/// past the missing `.git`, land on the main repo's own `.git`, and read
/// that (gitignored, so empty) status as clean, letting `require_clean`
/// pass and the purge run. The outright refusal never gets that far: no
/// probe runs at all for a prunable tree.
#[test]
fn rm_refuses_a_present_prunable_tree_under_an_in_repo_gitignored_root() {
    let env = Env::new("prunable-inrepo-root");
    std::fs::write(env.repo().join(".env"), "SECRET=1").unwrap();
    std::fs::write(env.repo().join(".gitignore"), ".env\n.worktrees/\n").unwrap();
    env.git(&["add", ".gitignore"]);
    env.git(&["commit", "-qm", "ignore .worktrees"]);
    env.set_config("[mode.new]\nroot = \".worktrees\"\n[defaults]\ncopy = [\".env\"]\n");

    let path = new_tree(&env, "feat/leak");
    assert!(
        path.starts_with(env.repo()),
        "the tree must land inside the repo checkout: {}",
        path.display()
    );
    assert!(path.join(".env").is_file());
    delete_only_dot_git(&path);

    let out = env.wt(&["rm", "feat/leak"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "");
    assert!(
        Env::stderr(&out).contains("its .git link is broken"),
        "{}",
        Env::stderr(&out)
    );
    assert!(path.join(".env").is_file(), "nothing purged");
    assert!(path.join("f.txt").is_file());
    assert_eq!(env.trees().len(), 1, "still registered (prunable)");
}

/// A prunable tree with no sidecar follows the same rule as any other tree:
/// refused without `--force`, whether or not its directory is actually gone.
#[test]
fn rm_refuses_a_prunable_tree_with_no_sidecar_without_force() {
    let env = Env::new("prunable-nometa");
    let path = env.worktrees_root().join("feat/nometa");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    env.git(&[
        "worktree",
        "add",
        "-b",
        "feat/nometa",
        path.to_str().unwrap(),
        "main",
    ]);
    delete_by_hand(&path);

    let out = env.wt(&["rm", "feat/nometa"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).contains("no readable wt metadata"),
        "{}",
        Env::stderr(&out)
    );
    assert_eq!(env.trees().len(), 1, "still registered (prunable)");
}

/// A sweep touching two prunable (and gone) trees at once: one whose branch
/// is merged is reaped outright, the other's unmerged branch survives the
/// same `if_merged` safety net a single `rm` already honours.
#[test]
fn stale_sweep_reaps_one_prunable_tree_and_keeps_the_other_unmerged_branch() {
    let env = Env::new("prunable-sweep-mixed");
    let reapable = new_tree(&env, "feat/reapable");
    let unmerged = new_tree(&env, "feat/keepbranch");
    commit_in(&env, &unmerged);
    age_sidecar(&env, &reapable, 9);
    age_sidecar(&env, &unmerged, 9);
    env.set_config("[teardown.mode.new]\nttl_days = 3\n");
    delete_by_hand(&reapable);
    delete_by_hand(&unmerged);

    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new(), "both registrations gone");
    assert_eq!(env.sidecars(), Vec::<String>::new(), "both sidecars gone");
    assert!(!env.branches().contains(&"feat/reapable".to_owned()));
    assert!(
        env.branches().contains(&"feat/keepbranch".to_owned()),
        "if_merged's -d must not lose unmerged work just because the tree was reaped"
    );
    assert!(
        Env::stderr(&out).contains("kept branch feat/keepbranch: "),
        "{}",
        Env::stderr(&out)
    );
}

/// A sweep never even asks whether a present-prunable tree is stale/merged:
/// `skip_broken_prunable` filters it out first, with its own message.
#[test]
fn stale_sweep_skips_a_present_prunable_tree_with_its_own_message() {
    let env = seeded_env("prunable-sweep-broken-skip");
    let path = new_tree(&env, "feat/brokenlink");
    delete_only_dot_git(&path);

    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).contains("skipped brokenlink: its .git link is broken"),
        "{}",
        Env::stderr(&out)
    );
    assert!(path.join(".env").is_file(), "nothing purged");
    assert_eq!(env.trees().len(), 1, "left registered, not attempted");
}

#[test]
fn stale_sweep_reaps_past_ttl() {
    let env = Env::new("prunable-sweep-stale");
    let path = new_tree(&env, "feat/aged");
    age_sidecar(&env, &path, 9);
    env.set_config("[teardown.mode.new]\nttl_days = 3\n");
    delete_by_hand(&path);

    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new());
    assert_eq!(env.sidecars(), Vec::<String>::new());
}

#[test]
fn merged_sweep_reaps_a_hand_deleted_tree() {
    let env = Env::new("prunable-sweep-merged");
    let path = new_tree(&env, "feat/landed");
    commit_in(&env, &path);
    merge_into_main(&env, "feat/landed");
    delete_by_hand(&path);

    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(env.trees(), Vec::<String>::new());
    assert_eq!(env.sidecars(), Vec::<String>::new());
}

#[test]
fn delete_branch_always_refuses_a_hand_deleted_tree_with_unpushed_commits() {
    let env = Env::new("prunable-always");
    env.set_config("[teardown.mode.new]\ndelete_branch = \"always\"\n");
    let path = new_tree(&env, "feat/always");
    commit_in(&env, &path);
    delete_by_hand(&path);

    // The tree is gone, but its branch holds the only copy of the work:
    // `branch -D` must not destroy it unasked.
    let out = env.wt(&["rm", "feat/always"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).contains("unpushed commits"),
        "{}",
        Env::stderr(&out)
    );
    assert!(env.branches().contains(&"feat/always".to_owned()));

    let out = env.wt(&["rm", "--force", "feat/always"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!env.branches().contains(&"feat/always".to_owned()));
    assert_eq!(env.trees(), Vec::<String>::new());
}
