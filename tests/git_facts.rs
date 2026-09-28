mod common;

use std::path::Path;

use common::Env;

/// Fix round 1, finding 1: `git rev-parse --git-common-dir` returns a path
/// shaped by cwd (relative, or absolute in cwd's own form); `git worktree
/// list --porcelain` always reports git's own canonical form. Running from
/// anywhere other than the repo root used to make `main_root` compare
/// unequal to the porcelain path for the very same directory, so `(main)`
/// vanished from `ls`, `complete` offered the main tree, and `cd` printed
/// the wrong thing.

#[test]
fn ls_finds_the_main_row_from_a_subdirectory() {
    let env = Env::new("facts-subdir-ls");
    env.make_tree("feat/one", "new");
    let sub = env.repo().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = env.wt_at(&sub, &["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stdout = Env::stdout(&out);
    assert!(
        stdout.lines().next().unwrap().starts_with("(main)"),
        "{stdout}"
    );
    assert!(stdout.contains("feat/one"), "{stdout}");
}

#[test]
fn cd_from_a_subdirectory_still_resolves_the_main_worktree() {
    let env = Env::new("facts-subdir-cd");
    let sub = env.repo().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = env.wt_at(&sub, &["cd"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Path::new(Env::stdout(&out).trim()), env.repo());
}

#[test]
fn complete_from_a_subdirectory_still_omits_the_main_tree() {
    let env = Env::new("facts-subdir-complete");
    env.make_tree("feat/one", "new");
    let sub = env.repo().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let out = env.wt_at(&sub, &["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let names: Vec<String> = Env::stdout(&out).lines().map(str::to_owned).collect();
    assert!(!names.iter().any(|n| n == "(main)"), "{names:?}");
    assert!(names.contains(&"feat/one".to_owned()), "{names:?}");
}

#[test]
fn ls_from_inside_a_linked_worktree_still_finds_the_main_row() {
    let env = Env::new("facts-linked-ls");
    let tree = env.make_tree("feat/one", "new");
    let out = env.wt_at(&tree, &["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stdout = Env::stdout(&out);
    assert!(
        stdout.lines().next().unwrap().starts_with("(main)"),
        "{stdout}"
    );
    assert!(stdout.contains("feat/one"), "{stdout}");
}

#[test]
fn cd_from_inside_a_linked_worktree_resolves_the_main_root_not_itself() {
    let env = Env::new("facts-linked-cd");
    let tree = env.make_tree("feat/one", "new");
    let out = env.wt_at(&tree, &["cd"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Path::new(Env::stdout(&out).trim()), env.repo());
}

#[test]
fn complete_from_inside_a_linked_worktree_omits_the_main_tree() {
    let env = Env::new("facts-linked-complete");
    let tree = env.make_tree("feat/one", "new");
    let out = env.wt_at(&tree, &["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let names: Vec<String> = Env::stdout(&out).lines().map(str::to_owned).collect();
    assert!(!names.iter().any(|n| n == "(main)"), "{names:?}");
    assert!(names.contains(&"feat/one".to_owned()), "{names:?}");
}

// The exact repro the review gave: an 8.3 short-name cwd
// (`C:\Users\BJORNK~1.PUN\...`). `git rev-parse --git-common-dir` from a
// short-name cwd returns a *relative* `.git`, which then joins onto the
// short-name cwd itself; `git worktree list --porcelain` always reports the
// long canonical form, so the two never compared equal.

#[cfg(windows)]
#[test]
fn ls_from_a_short_name_cwd_still_finds_the_main_row() {
    let env = Env::new("facts-shortname-ls");
    env.make_tree("feat/one", "new");
    let short_repo = common::short_path(&env.repo());
    let out = env.wt_at(&short_repo, &["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stdout = Env::stdout(&out);
    assert!(
        stdout.lines().next().unwrap().starts_with("(main)"),
        "{stdout}"
    );
    assert!(stdout.contains("feat/one"), "{stdout}");
}

#[cfg(windows)]
#[test]
fn cd_from_a_short_name_cwd_resolves_the_long_form_main_root() {
    let env = Env::new("facts-shortname-cd");
    let short_repo = common::short_path(&env.repo());
    let out = env.wt_at(&short_repo, &["cd"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Path::new(Env::stdout(&out).trim()), env.repo());
}
