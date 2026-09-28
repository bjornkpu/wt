// Marks this file as test code, so clippy.toml allows unwrap in its helpers too.
#![cfg(test)]

mod common;

use common::Env;

/// An origin with a `feat/r` branch one commit ahead of main; returns its sha.
fn remote_branch(env: &Env) -> String {
    move_remote_branch(env, "HEAD")
}

/// Pushes a new commit on top of `parent` to origin's `feat/r`.
fn move_remote_branch(env: &Env, parent: &str) -> String {
    let sha = env.git(&["commit-tree", "HEAD^{tree}", "-p", parent, "-m", "r"]);
    env.git(&[
        "push",
        "-q",
        "-f",
        "origin",
        &format!("{sha}:refs/heads/feat/r"),
    ]);
    sha
}

fn sidecar(env: &Env, leaf: &str) -> serde_json::Value {
    let name = leaf.replace('/', "%2F");
    let file = env.repo().join(".git/wt").join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
}

#[test]
fn a_detached_branch_tree_records_the_resolved_sha() {
    let env = Env::new("branch-detached");
    let sha = remote_branch(&env);
    let out = env.wt(&["branch", "feat/r"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("review").join("feat").join("r");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert_eq!(env.git_at(&path, &["rev-parse", "HEAD"]), sha);
    assert_eq!(env.git_at(&path, &["branch", "--show-current"]), "");

    let meta = sidecar(&env, "review/feat/r");
    assert_eq!(meta["mode"], "branch");
    assert_eq!(
        meta["base"],
        sha.as_str(),
        "F17: the sha, not origin/feat/r"
    );
    assert_eq!(meta["branch"], serde_json::Value::Null);
    assert_eq!(meta["detached"], true);
}

#[test]
fn f17_a_moved_remote_branch_is_refused_like_a_moved_pr() {
    let env = Env::new("branch-moved");
    let old = remote_branch(&env);
    assert!(env.wt(&["-q", "branch", "feat/r"]).status.success());
    let new = move_remote_branch(&env, &old);

    let out = env.wt(&["branch", "feat/r"]);
    assert!(!out.status.success());
    let path = env.worktrees_root().join("review").join("feat").join("r");
    assert_eq!(
        Env::stderr(&out),
        format!(
            "wt: {} is pinned to {} but branch now resolves to {}; remove it and re-create\n",
            path.display(),
            old.get(..8).unwrap(),
            new.get(..8).unwrap()
        )
    );
}

#[test]
fn f17_an_old_sidecar_with_a_ref_base_is_reused() {
    let env = Env::new("branch-old-sidecar");
    let old = remote_branch(&env);
    assert!(env.wt(&["-q", "branch", "feat/r"]).status.success());
    // What the Python tool wrote: the ref name, not a sha.
    let file = env.repo().join(".git/wt/review%2Ffeat%2Fr.json");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace(&old, "origin/feat/r")).unwrap();
    move_remote_branch(&env, &old);

    let out = env.wt(&["branch", "feat/r"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).starts_with("exists: "),
        "{}",
        Env::stderr(&out)
    );
}

fn assert_tracking(env: &Env, out: &std::process::Output) {
    assert!(out.status.success(), "{}", Env::stderr(out));
    let path = env.worktrees_root().join("feat").join("r");
    assert_eq!(Env::stdout(out), format!("{}\n", path.display()));
    assert_eq!(env.git_at(&path, &["branch", "--show-current"]), "feat/r");
    assert_eq!(
        env.git(&["rev-parse", "--abbrev-ref", "feat/r@{u}"]),
        "origin/feat/r"
    );
    let meta = sidecar(env, "feat/r");
    assert_eq!(meta["branch"], "feat/r");
    assert_eq!(meta["detached"], false);
}

#[test]
fn track_checks_out_a_tracking_branch() {
    let env = Env::new("branch-track");
    remote_branch(&env);
    assert_tracking(&env, &env.wt(&["branch", "feat/r", "--track"]));
}

#[test]
fn f16_detach_false_without_track_behaves_as_track() {
    let env = Env::new("branch-detach-false");
    env.set_config("[mode.branch]\ndetach = false\n");
    remote_branch(&env);
    assert_tracking(&env, &env.wt(&["branch", "feat/r"]));
}

#[test]
fn a_missing_remote_branch_fails_before_anything_is_made() {
    let env = Env::new("branch-missing");
    let out = env.wt(&["branch", "feat/nope"]);
    assert!(!out.status.success());
    assert_eq!(Env::stderr(&out), "wt: no remote branch origin/feat/nope\n");
    assert_eq!(env.trees(), Vec::<String>::new());
    assert_eq!(env.sidecars(), Vec::<String>::new());
}

#[test]
fn pr_without_a_supported_provider_is_refused() {
    let env = Env::new("pr-none");
    let out = env.wt(&["pr", "1"]);
    assert!(!out.status.success());
    assert_eq!(
        Env::stderr(&out),
        "wt: no supported provider on origin; cannot resolve a PR id\n"
    );
    assert_eq!(env.trees(), Vec::<String>::new());
}

#[test]
fn run_without_open_is_refused_on_pr_and_branch() {
    let env = Env::new("pr-branch-run");
    for verb in ["pr", "branch"] {
        let out = env.wt(&[verb, "1", "--run", "x"]);
        assert!(!out.status.success());
        assert_eq!(
            Env::stderr(&out),
            "wt: --run has nowhere to run without --open\n"
        );
    }
}
