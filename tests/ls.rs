mod common;

use common::Env;

/// The pure rendering is snapshot-tested directly in `view.rs`; this checks
/// the wiring end to end: git facts gathered, sidecar read, table printed
/// on stdout.
#[test]
fn ls_lists_the_main_tree_and_a_worktree() {
    let env = Env::new("ls-smoke");
    env.make_tree("feat/one", "new");
    let out = env.wt(&["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stdout = Env::stdout(&out);
    let mut lines = stdout.lines();
    let main_line = lines.next().unwrap();
    assert!(main_line.starts_with("(main)"), "{main_line}");
    assert!(main_line.contains("main"), "{main_line}");
    let tree_line = lines.next().unwrap();
    assert!(tree_line.starts_with("feat/one"), "{tree_line}");
    assert!(tree_line.contains("feat/one"), "{tree_line}");
    assert!(tree_line.contains("new"), "{tree_line}");
    assert_eq!(lines.next(), None);
}

#[test]
fn ls_prints_nothing_for_a_bare_repo() {
    // A bare repo has no worktree records at all (not even a main one).
    let root = std::env::temp_dir().join(format!("wt-it-ls-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let wt_home = root.join("wt-home");
    std::fs::create_dir_all(&wt_home).unwrap();
    let gitconfig = root.join("gitconfig");
    std::fs::write(&gitconfig, "").unwrap();
    let bare = root.join("bare.git");
    std::process::Command::new("git")
        .args(["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()])
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_wt"))
        .args(["ls"])
        .current_dir(&bare)
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("WT_HOME", &wt_home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(&root);
}
