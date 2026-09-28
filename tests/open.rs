mod common;

use common::Env;

// -------------------------------------------------------------------- TestOpen

#[test]
fn open_without_herdr_is_a_skip_and_prints_nothing() {
    let env = Env::new("open-skip");
    env.make_tree("feat/target", "new");
    let out = env.wt(&["open", "target", "--run", "claude"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "", "the wrapper must not cd anywhere");
    assert_eq!(Env::stderr(&out), "herdr not on PATH, skipped\n");
}

#[test]
fn test_an_unknown_name_is_refused() {
    let env = Env::new("open-unknown");
    let out = env.wt(&["open", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(Env::stdout(&out), "");
    assert_eq!(Env::stderr(&out), "wt: no worktree matching 'nope'\n");
}

#[test]
fn f12_a_failed_open_exits_1() {
    let env = Env::new("open-fails");
    env.make_tree("feat/target", "new");
    env.set_config("[herdr]\nlabel_default = \"{slug}\"\n");
    let out = env.wt(&["open", "target"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), "");
}

#[test]
fn branch_and_pr_open_without_herdr_still_print_the_path() {
    let env = Env::new("branch-open");
    env.git(&["push", "-q", "origin", "main:review-me"]);
    env.git(&["fetch", "-q", "origin"]);
    let out = env.wt(&["branch", "review-me", "--open"]);
    let stderr = Env::stderr(&out);
    assert!(out.status.success(), "{stderr}");
    let path = env.worktrees_root().join("review").join("review-me");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert!(
        stderr.contains("  herdr not on PATH, skipped\n"),
        "{stderr}"
    );
}

#[test]
fn test_opens_the_tree_by_bare_name_labelled_through_its_sidecar_mode() {
    // Only the pr label is broken, so the exit code says which mode's label
    // was used: the sidecar's, not a hard-coded "new".
    let env = Env::new("open-mode");
    env.make_tree("feat/as-pr", "pr");
    env.make_tree("feat/as-new", "new");
    env.set_config("[herdr.label]\npr = \"{slug}\"\n");

    let pr = env.wt(&["open", "as-pr"]);
    assert_eq!(pr.status.code(), Some(1), "{}", Env::stderr(&pr));
    assert!(
        Env::stderr(&pr).contains("unresolved placeholder"),
        "{}",
        Env::stderr(&pr)
    );

    let new = env.wt(&["open", "as-new"]);
    assert!(new.status.success(), "{}", Env::stderr(&new));
    assert_eq!(Env::stderr(&new), "herdr not on PATH, skipped\n");
}

#[test]
fn an_empty_run_is_absent_not_refused() {
    let env = Env::new("open-empty-run");
    let out = env.wt(&["new", "feat/empty-run", "--run", ""]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("empty-run");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
}
