// Test code throughout, so clippy.toml allows unwrap in the helpers too.
#![cfg(test)]

mod common;

use std::path::{Path, PathBuf};

use common::Env;

// --------------------------------------------------------------- helpers

fn commit_in(env: &Env, path: &Path) {
    std::fs::write(path.join("wip.txt"), "x").unwrap();
    env.git_at(path, &["add", "wip.txt"]);
    env.git_at(path, &["commit", "-qm", "wip"]);
}

/// Fast-forwards main (and its origin) onto `branch`'s tip, the way a
/// landed PR leaves the default branch: an ancestry match `--merged` sees
/// directly, no upstream tracking involved.
fn merge_into_main(env: &Env, branch: &str) {
    env.git(&["merge", "-q", "--ff-only", branch]);
    env.git(&["push", "-q", "origin", "main"]);
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

fn days_ago(days: i64) -> String {
    let now = jiff::Timestamp::now().as_second();
    let secs = now.checked_sub(days.checked_mul(86_400).unwrap()).unwrap();
    jiff::Timestamp::from_second(secs).unwrap().to_string()
}

/// Rewrites the sidecar `make_tree`/`make_detached_tree` wrote for `path`:
/// `mode` (so a `[teardown.mode.X]` ttl applies) and `created`, aged by
/// `days`.
fn age_sidecar(env: &Env, path: &Path, days: i64, mode: &str) {
    let file = find_sidecar(env, path);
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    v["mode"] = serde_json::json!(mode);
    v["created"] = serde_json::json!(days_ago(days));
    std::fs::write(&file, v.to_string()).unwrap();
}

// ------------------------------------------------------- TestRemoveMerged

#[test]
fn f1_a_fresh_branch_is_spared_even_though_it_matches_merged() {
    // Flips the Python original's test_reaps_a_branch_that_has_landed:
    // a branch with no commits of its own only "matches" --merged because
    // it is identical to the default branch (hazard #24 / F1).
    let env = Env::new("sweep-fresh");
    let path = env.make_tree("feat/landed", "new");
    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(path.exists(), "a fresh branch must survive --merged");
    assert!(
        Env::stderr(&out).contains("skipped landed: no commits ever landed on it"),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn f2_a_fast_forwarded_fresh_branch_is_spared_not_reaped() {
    // A branch that never committed anything of its own, but whose owner
    // ran `git pull --ff-only` to catch up with a moved main: the reflog
    // gains a "pull ...: Fast-forward" entry on top of the creation entry,
    // which must not be mistaken for a real commit.
    let env = Env::new("sweep-ff-pull");
    let path = env.make_tree("feat/ff", "new");
    std::fs::write(env.repo().join("h.txt"), "moved on").unwrap();
    env.git(&["add", "h.txt"]);
    env.git(&["commit", "-qm", "main moved on"]);
    env.git(&["push", "-q", "origin", "main"]);
    env.git_at(&path, &["pull", "-q", "--ff-only", "origin", "main"]);

    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        path.exists(),
        "a fast-forwarded fresh branch must survive --merged"
    );
    assert!(
        Env::stderr(&out).contains("skipped ff: no commits ever landed on it"),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn a_merged_branch_with_real_commits_is_reaped() {
    let env = Env::new("sweep-real-merge");
    let path = env.make_tree("feat/landed2", "new");
    commit_in(&env, &path);
    merge_into_main(&env, "feat/landed2");
    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}

#[test]
fn test_spares_a_branch_with_unmerged_work() {
    let env = Env::new("sweep-inflight");
    let path = env.make_tree("feat/inflight", "new");
    commit_in(&env, &path);
    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(path.exists(), "unmerged work must survive the sweep");
    assert!(Env::stderr(&out).contains("nothing to remove"));
}

#[test]
fn test_reaps_a_squash_merged_branch_whose_upstream_is_gone() {
    let env = Env::new("sweep-squashed");
    let path = env.make_tree("feat/squashed", "new");
    commit_in(&env, &path);
    let head = env.git_at(&path, &["rev-parse", "HEAD"]);
    let reference = "refs/remotes/origin/feat/squashed";
    env.git(&["update-ref", reference, &head]);
    env.git(&[
        "branch",
        "--set-upstream-to",
        "origin/feat/squashed",
        "feat/squashed",
    ]);
    env.git(&["update-ref", "-d", reference]);
    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists(), "a gone upstream means the PR landed");
}

#[test]
fn test_spares_a_detached_review_tree() {
    let env = Env::new("sweep-detached");
    let path = env.make_detached_tree("review/pr-1", "pr");
    let out = env.wt(&["rm", "--merged"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(path.exists());
    assert!(
        Env::stderr(&out).contains("skipped pr-1: detached, so it has no branch to merge"),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn test_stale_and_merged_together_are_refused() {
    // F20: clap refuses the combination before any fetch, as a usage error
    // (exit 2), not a `wt:` runtime error.
    let env = Env::new("sweep-conflict");
    let out = env.wt(&["rm", "--stale", "--merged"]);
    assert_eq!(out.status.code(), Some(2), "{}", Env::stderr(&out));
}

#[test]
fn f20_a_sweep_flag_together_with_a_name_is_also_refused() {
    let env = Env::new("sweep-conflict-name");
    let out = env.wt(&["rm", "--merged", "feat/x"]);
    assert_eq!(out.status.code(), Some(2), "{}", Env::stderr(&out));
}

// -------------------------------------------------------------- TestStale

#[test]
fn test_reaps_past_ttl() {
    let env = Env::new("sweep-old");
    let path = env.make_tree("feat/old", "pr");
    age_sidecar(&env, &path, 9, "pr");
    env.set_config("[teardown.mode.pr]\nttl_days = 3\nrequire_clean = false\n");
    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}

#[test]
fn test_spares_inside_ttl() {
    let env = Env::new("sweep-young");
    let path = env.make_tree("feat/young", "pr");
    age_sidecar(&env, &path, 1, "pr");
    env.set_config("[teardown.mode.pr]\nttl_days = 3\n");
    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(path.exists());
    assert!(Env::stderr(&out).contains("nothing to remove"));
}

#[test]
fn test_spares_modes_without_ttl() {
    let env = Env::new("sweep-forever");
    let path = env.make_tree("feat/forever", "new");
    age_sidecar(&env, &path, 400, "new");
    let out = env.wt(&["rm", "--stale"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(path.exists());
    assert!(
        Env::stderr(&out).contains("skipped forever: no ttl_days configured for mode 'new'"),
        "{}",
        Env::stderr(&out)
    );
}

// ------------------------------------------------------ TestSweepResilience

#[test]
fn test_sweep_continues_past_a_refusal() {
    let env = Env::new("sweep-resilience");
    let good = env.make_tree("feat/reapable", "pr");
    let bad = env.make_tree("feat/unreapable", "pr");
    age_sidecar(&env, &good, 9, "pr");
    age_sidecar(&env, &bad, 9, "pr");
    std::fs::write(bad.join("untracked.txt"), "x").unwrap();
    env.set_config("[teardown.mode.pr]\nttl_days = 3\n");

    let out = env.wt(&["rm", "--stale"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert!(!good.exists(), "the reapable tree should still be removed");
    assert!(bad.exists(), "the refusing tree must survive");
    let stderr = Env::stderr(&out);
    assert!(stderr.contains("left in place"), "{stderr}");
    assert!(
        stderr.contains("1 of 2 worktrees could not be removed"),
        "{stderr}"
    );
}
