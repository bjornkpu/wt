mod common;

use std::path::Path;

use common::Env;

/// `git worktree list --porcelain` reports paths with forward slashes even
/// on Windows; compare through `Path`, whose `Eq` is component-wise, rather
/// than as raw strings.
fn stdout_path_is(out: &std::process::Output, expected: &Path) {
    let stdout = Env::stdout(out);
    assert_eq!(Path::new(stdout.trim()), expected, "{stdout}");
}

// ------------------------------------------------------------------- TestCd

#[test]
fn test_resolves_a_worktree_by_leaf() {
    let env = Env::new("cd-leaf");
    let path = env.make_tree("feat/target", "new");
    let out = env.wt(&["cd", "feat/target"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    stdout_path_is(&out, &path);
}

#[test]
fn test_resolves_by_bare_name() {
    let env = Env::new("cd-bare");
    let path = env.make_tree("feat/target", "new");
    let out = env.wt(&["cd", "target"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    stdout_path_is(&out, &path);
}

#[test]
fn test_no_name_is_the_main_worktree() {
    let env = Env::new("cd-none");
    env.make_tree("feat/target", "new");
    let out = env.wt(&["cd"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    stdout_path_is(&out, &env.repo());
}

#[test]
fn test_an_unknown_name_is_refused() {
    let env = Env::new("cd-unknown");
    let out = env.wt(&["cd", "nope"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    assert!(
        Env::stderr(&out).contains("no worktree matching"),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn test_an_ambiguous_name_is_refused_not_guessed() {
    let env = Env::new("cd-ambiguous");
    env.make_tree("feat/dup", "new");
    env.make_tree("fix/dup", "new");
    let out = env.wt(&["cd", "dup"]);
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(1));
    assert!(
        Env::stderr(&out).contains("matches 2"),
        "{}",
        Env::stderr(&out)
    );
}

// -------------------------------------------------------------- TestComplete

#[test]
fn test_offers_the_leaf_and_omits_the_main_tree() {
    let env = Env::new("complete-leaf");
    env.make_tree("feat/one", "new");
    let out = env.wt(&["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let names: Vec<String> = Env::stdout(&out).lines().map(str::to_owned).collect();
    assert!(names.contains(&"feat/one".to_owned()), "{names:?}");
    assert!(!names.iter().any(|n| n == "(main)"), "{names:?}");
}

#[test]
fn test_every_name_offered_actually_resolves() {
    let env = Env::new("complete-resolves");
    env.make_tree("feat/one", "new");
    env.make_tree("fix/two", "new");
    let out = env.wt(&["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    for name in Env::stdout(&out).lines() {
        let cd = env.wt(&["cd", name]);
        assert!(cd.status.success(), "{name}: {}", Env::stderr(&cd));
    }
}

#[test]
fn test_a_detached_tree_is_offered_by_its_directory() {
    let env = Env::new("complete-detached");
    env.make_detached_tree("review/pr-7", "pr");
    let out = env.wt(&["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let names: Vec<String> = Env::stdout(&out).lines().map(str::to_owned).collect();
    assert!(names.contains(&"review/pr-7".to_owned()), "{names:?}");
}

// ------------------------------------------------------ corrupt sidecar (F9/item 7)
//
// A sidecar that is valid JSON but not an object (`parse::SidecarRead::NotObject`),
// written directly (not via a shared non-`#[test]` helper: clippy's
// `unwrap_used`/`expect_used` test exemption only covers code inside a
// `#[test]` function).

#[test]
fn test_ls_still_warns_about_a_corrupt_sidecar() {
    let env = Env::new("corrupt-ls-warns");
    let dir = env.repo().join(".git").join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.json"), "[]").unwrap();
    let out = env.wt(&["ls"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).contains("sidecar is not a JSON object"),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn test_complete_is_quiet_about_a_corrupt_sidecar() {
    let env = Env::new("corrupt-complete-quiet");
    env.make_tree("feat/one", "new");
    let dir = env.repo().join(".git").join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.json"), "[]").unwrap();
    let out = env.wt(&["complete"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stderr(&out), "");
}

// ------------------------------------------------- dynamic completion engine (item 6)

#[test]
fn test_completion_engine_lists_the_tree_and_is_quiet_about_a_corrupt_sidecar() {
    // `COMPLETE=powershell wt -- wt rm ''`: the shape `clap_complete`'s
    // dynamic engine (`CompleteEnv`) expects - argv after `--` is the line
    // being completed, its own name standing in for the binary. This drives
    // the same `ArgValueCompleter` a real pwsh/zsh tab press would.
    let env = Env::new("complete-engine");
    env.make_tree("feat/one", "new");
    let dir = env.repo().join(".git").join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bad.json"), "[]").unwrap();
    let out = env
        .command(env!("CARGO_BIN_EXE_wt"), &env.repo())
        .env("COMPLETE", "powershell")
        .args(["--", "wt", "rm", ""])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let names: Vec<String> = Env::stdout(&out).lines().map(str::to_owned).collect();
    assert!(names.contains(&"feat/one".to_owned()), "{names:?}");
    assert_eq!(Env::stderr(&out), "");
}

#[test]
fn a_closed_stdout_is_a_quiet_exit_not_a_panic() {
    let env = Env::new("cd-closed-stdout");
    let mut child = env
        .command(env!("CARGO_BIN_EXE_wt"), &env.repo())
        .arg("cd")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Nobody reads the path: the pipe is closed before wt writes it.
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", Env::stderr(&out));
}

#[test]
fn a_log_dir_that_cannot_be_made_only_warns() {
    let env = Env::new("cd-no-log-dir");
    let blocker = env.root.join("a-file");
    std::fs::write(&blocker, "").unwrap();
    let out = env
        .command(env!("CARGO_BIN_EXE_wt"), &env.repo())
        .env("WT_HOME", blocker.join("home"))
        .arg("cd")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(
        Env::stdout(&out).trim_end(),
        env.repo().display().to_string()
    );
    let stderr = Env::stderr(&out);
    assert!(stderr.starts_with("wt: logging is off: "), "{stderr}");
    assert!(stderr.contains(&blocker.display().to_string()), "{stderr}");
}
