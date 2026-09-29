mod common;

use std::time::{Duration, Instant};

use common::Env;

const SLEEP: &str = if cfg!(windows) {
    "Start-Sleep -Seconds 30"
} else {
    "sleep 30"
};

fn nothing_left(env: &Env, branch: &str) {
    assert_eq!(env.trees(), Vec::<String>::new(), "a tree was left behind");
    assert!(
        !env.branches().iter().any(|b| b == branch),
        "the branch outlived its worktree"
    );
    assert_eq!(
        env.sidecars(),
        Vec::<String>::new(),
        "a sidecar was left behind"
    );
}

// ------------------------------------------------------------- TestAcceptance

#[test]
fn test_new_prints_the_exact_path_seeds_and_ls_shows_it() {
    let env = Env::new("new-acceptance");
    env.set_config("[defaults]\ncopy = [\".env\"]\n");
    std::fs::write(env.repo().join(".env"), "SECRET=1").unwrap();

    let out = env.wt(&["-q", "new", "feat/acceptance-probe", "--no-llm"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("acceptance-probe");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert_eq!(Env::stderr(&out), "", "-q: nothing but failures on stderr");
    assert!(path.join(".env").is_file(), "configured copy was seeded");
    assert_eq!(env.sidecars(), ["feat%2Facceptance-probe.json"]);

    let ls = env.wt(&["ls"]);
    let listing = Env::stdout(&ls);
    let row = listing
        .lines()
        .find(|l| l.starts_with("feat/acceptance-probe"))
        .unwrap_or_else(|| panic!("{listing}"));
    assert!(
        row.contains(" new "),
        "ls shows the mode from the sidecar: {row}"
    );
    assert!(!row.contains("dirty"), "{row}");
}

#[test]
fn prose_is_named_mechanically_and_flags_win() {
    let env = Env::new("new-prose");
    let out = env.wt(&["new", "Går på økt"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stdout(&out).trim_end().ends_with("gar-okt"),
        "{}",
        Env::stdout(&out)
    );
    assert!(env.branches().contains(&"feat/gar-okt".to_owned()));

    let out = env.wt(&["new", "whatever words", "--type", "fix", "--slug", "x"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(env.branches().contains(&"fix/x".to_owned()));
}

#[test]
fn f15_type_and_slug_are_validated() {
    let env = Env::new("new-f15");
    assert_eq!(
        env.wt(&["new", "x y", "--type", "feature"]).status.code(),
        Some(2)
    );
    assert_eq!(
        env.wt(&["new", "x y", "--slug", "Bad_Slug"]).status.code(),
        Some(2)
    );
    assert_eq!(env.trees(), Vec::<String>::new());
}

#[test]
fn run_without_open_is_refused() {
    let env = Env::new("new-run");
    let out = env.wt(&["new", "feat/x", "--run", "claude"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        Env::stderr(&out),
        "wt: --run has nowhere to run without --open\n"
    );
}

#[test]
fn an_existing_tree_is_handed_back() {
    let env = Env::new("new-exists");
    let first = env.wt(&["new", "feat/x"]);
    assert!(first.status.success(), "{}", Env::stderr(&first));
    let again = env.wt(&["new", "feat/x"]);
    assert!(again.status.success(), "{}", Env::stderr(&again));
    assert_eq!(Env::stdout(&again), Env::stdout(&first));
    assert!(
        Env::stderr(&again).contains(&format!("exists: {}", Env::stdout(&first).trim_end())),
        "{}",
        Env::stderr(&again)
    );
}

#[test]
fn a_failed_fetch_warns_and_carries_on() {
    let env = Env::new("new-fetch");
    let missing = env.root.join("missing.git");
    env.git(&["remote", "set-url", "origin", missing.to_str().unwrap()]);
    let out = env.wt(&["new", "feat/x"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stderr(&out).contains("wt: fetch failed, working from possibly stale refs: "),
        "{}",
        Env::stderr(&out)
    );
}

// --------------------------------------------------------- TestPathContainment

#[test]
fn test_refuses_to_create_outside_the_root() {
    let env = Env::new("new-contain");
    env.set_config("[mode.new]\nbranch = \"{type}/../../../{slug}\"\n");
    let out = env.wt(&["new", "escape attempt"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        Env::stderr(&out).starts_with("wt: refusing to create outside "),
        "{}",
        Env::stderr(&out)
    );
    assert!(Env::stdout(&out).is_empty());
    assert!(!env.root.join("escape-attempt").exists());
    assert!(!env.root.parent().unwrap().join("escape-attempt").exists());
}

// ---------------------------------------------------------- TestCreateRollback

#[test]
fn test_a_failed_seed_leaves_no_worktree() {
    let env = Env::new("new-rollback");
    // origin/main has a file `blocker`; the main checkout has a directory
    // there instead, so copying it into the new tree cannot succeed. `.env`
    // is copied first, so the failure lands part-way through the seed.
    std::fs::write(env.repo().join("blocker"), "tracked").unwrap();
    env.git(&["add", "blocker"]);
    env.git(&["commit", "-qm", "blocker"]);
    env.git(&["push", "-q", "origin", "main"]);
    env.git(&["rm", "-q", "blocker"]);
    std::fs::create_dir_all(env.repo().join("blocker")).unwrap();
    std::fs::write(env.repo().join("blocker/secret"), "s").unwrap();
    std::fs::write(env.repo().join(".env"), "SECRET=1").unwrap();
    env.set_config("[defaults]\ncopy = [\".env\", \"blocker\"]\n");

    let out = env.wt(&["new", "feat/halfdone"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert!(Env::stdout(&out).is_empty());
    let stderr = Env::stderr(&out);
    assert!(stderr.contains("wt: rolling back "), "{stderr}");
    assert!(stderr.contains("could not seed blocker"), "{stderr}");
    nothing_left(&env, "feat/halfdone");
    assert!(!env.worktrees_root().join("feat").join("halfdone").exists());
}

// ------------------------------------------------------------ TestExecContract

#[test]
fn test_exec_output_never_reaches_stdout() {
    let env = Env::new("new-exec-noise");
    env.set_config("[defaults]\nexec = [\"echo NOISE\"]\n");
    let out = env.wt(&["new", "feat/noisy"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("noisy");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    let stderr = Env::stderr(&out);
    assert!(stderr.contains("  exec: echo NOISE"), "{stderr}");
    assert!(
        stderr.contains("NOISE\n") || stderr.contains("NOISE\r\n"),
        "{stderr}"
    );
}

#[test]
fn test_a_hanging_exec_step_is_bounded() {
    let env = Env::new("new-exec-hang");
    env.set_config(&format!(
        "[defaults]\nexec = [\"{SLEEP}\"]\nexec_timeout = 1\n"
    ));
    let started = Instant::now();
    let out = env.wt(&["new", "feat/hangs"]);
    assert!(started.elapsed() < Duration::from_secs(20), "not bounded");
    // Spec section 4: the tree exists, so its path is still printed, and
    // the failed step makes the exit 1.
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("hangs");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    let stderr = Env::stderr(&out);
    assert!(
        stderr.contains(&format!("wt: exec timed out after 1s: {SLEEP}")),
        "{stderr}"
    );
    assert!(path.is_dir(), "the worktree survives a failed exec step");
    let sidecar = std::fs::read_to_string(env.repo().join(".git/wt/feat%2Fhangs.json")).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
    assert_eq!(meta["exec_failed"], serde_json::json!([SLEEP]));
}

#[test]
fn test_exec_strict_rolls_the_worktree_back() {
    let env = Env::new("new-exec-strict");
    env.set_config("[defaults]\nexec = [\"exit 1\"]\nexec_strict = true\n");
    let out = env.wt(&["new", "feat/strict"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        Env::stdout(&out).is_empty(),
        "nothing was created, so nothing to announce"
    );
    let stderr = Env::stderr(&out);
    assert!(
        stderr.contains("wt: exec failed (rc=1): exit 1"),
        "{stderr}"
    );
    assert!(
        stderr.contains("(output above; worktree rolled back)"),
        "{stderr}"
    );
    nothing_left(&env, "feat/strict");
}

#[test]
fn test_defer_exec_leaves_the_steps_for_the_new_workspace() {
    let env = Env::new("new-exec-defer");
    let marker = env.root.join("exec-ran");
    env.set_config(&format!(
        "[defaults]\nexec = [\"echo ran > '{}'\"]\nexec_strict = true\n",
        marker.display().to_string().replace('\\', "/")
    ));
    let out = env.wt(&["new", "feat/deferred", "--open", "--run", "claude"]);
    let stderr = Env::stderr(&out);
    // A missing herdr is a skip, not a failure: the tree is there as asked.
    assert!(out.status.success(), "{stderr}");
    let path = env.worktrees_root().join("feat").join("deferred");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert!(!marker.exists(), "exec ran here as well as over there");
    assert!(
        stderr.contains(
            "wt: exec runs in the new workspace, so exec_strict cannot roll this tree back\n"
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains("  herdr not on PATH, skipped\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("wt: herdr not on PATH; exec steps did not run: 1\n"),
        "{stderr}"
    );
}

#[test]
fn f7_a_symlinked_entry_is_recorded_for_teardown() {
    let env = Env::new("new-f7-symlink");
    std::fs::create_dir_all(env.repo().join("shared")).unwrap();
    env.set_config("[defaults]\nsymlink = [\"shared\"]\n");
    let out = env.wt(&["-q", "new", "feat/linked"]);
    // Windows without Developer Mode cannot make a link at all
    // (ERROR_PRIVILEGE_NOT_HELD): nothing to record then.
    if Env::stderr(&out).contains("os error 1314") {
        return;
    }
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("linked");
    assert!(path.join("shared").symlink_metadata().unwrap().is_symlink());
    let sidecar = std::fs::read_to_string(env.repo().join(".git/wt/feat%2Flinked.json")).unwrap();
    let meta: serde_json::Value = serde_json::from_str(&sidecar).unwrap();
    assert_eq!(meta["copied"], serde_json::json!(["shared"]));
}

/// A junction needs no privilege to make, but recreating it as a link does:
/// without Developer Mode its content is copied instead of rolling back.
#[cfg(windows)]
#[test]
fn a_junction_inside_a_copied_dir_does_not_fail_the_seed() {
    let env = Env::new("new-junction");
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("b.txt"), "b").unwrap();
    let dir = env.repo().join("dir");
    std::fs::create_dir_all(&dir).unwrap();
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(dir.join("j"))
        .arg(&outside)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    env.set_config("[defaults]\ncopy = [\"dir\"]\n");

    let out = env.wt(&["-q", "new", "feat/junction"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let path = env.worktrees_root().join("feat").join("junction");
    assert_eq!(
        std::fs::read_to_string(path.join("dir").join("j").join("b.txt")).unwrap(),
        "b"
    );
}

#[test]
fn a_rollback_keeps_a_local_branch_it_did_not_create() {
    let env = Env::new("new-rollback-keeps-branch");
    env.git(&["branch", "feat/existing"]);
    // `blocker` is a file in the tree and a directory in the main checkout,
    // so the seed fails after the tree exists (as in the rollback test above).
    std::fs::write(env.repo().join("blocker"), "tracked").unwrap();
    env.git(&["add", "blocker"]);
    env.git(&["commit", "-qm", "blocker"]);
    env.git(&["branch", "-f", "feat/existing"]);
    env.git(&["rm", "-q", "blocker"]);
    std::fs::create_dir_all(env.repo().join("blocker")).unwrap();
    std::fs::write(env.repo().join("blocker/secret"), "s").unwrap();
    env.set_config("[defaults]\ncopy = [\"blocker\"]\n");

    let out = env.wt(&["new", "feat/existing"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert!(Env::stderr(&out).contains("wt: rolling back "));
    assert_eq!(env.trees(), Vec::<String>::new());
    assert!(
        env.branches().contains(&"feat/existing".to_owned()),
        "the rollback deleted a branch that was there before wt ran"
    );
}

#[test]
fn f12_a_failed_open_prints_the_path_and_exits_1() {
    // A label typo fails the open after the tree exists: the partial-failure
    // protocol, on a machine with or without herdr.
    let env = Env::new("new-open-fails");
    env.set_config("[herdr]\nlabel_default = \"{slug}\"\n");
    let out = env.wt(&["new", "feat/badlabel", "--open"]);
    let stderr = Env::stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let path = env.worktrees_root().join("feat").join("badlabel");
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert!(
        stderr.contains(&format!(
            "wt: 1 post-create step(s) failed; the worktree is at {}",
            path.display()
        )),
        "{stderr}"
    );
    assert_eq!(env.trees().len(), 1, "the tree is kept, not rolled back");
}

/// Kills the exec step's background process when the test ends, pass or
/// fail, so it does not outlive the test holding handles.
struct KillOnDrop(std::path::PathBuf);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let Ok(pid) = std::fs::read_to_string(&self.0) else {
            return;
        };
        let pid = pid.trim();
        let _ = if cfg!(windows) {
            std::process::Command::new("taskkill")
                .args(["/F", "/T", "/PID", pid])
                .output()
        } else {
            std::process::Command::new("kill").arg(pid).output()
        };
    }
}

#[test]
fn a_backgrounded_grandchild_does_not_hold_the_callers_pipes() {
    // The step returns at once; what it started keeps running. A caller
    // that captures both of wt's streams (the shell wrapper, the
    // WorktreeCreate hook) must see them close when wt exits, not when the
    // background process does.
    let env = Env::new("new-exec-grandchild");
    let pid_file = env.root.join("bg.pid");
    let _kill = KillOnDrop(pid_file.clone());
    let pid_file = pid_file.display().to_string().replace('\\', "/");
    let spawn = if cfg!(windows) {
        format!(
            "$p = Start-Process -NoNewWindow -PassThru pwsh -ArgumentList '-NoProfile','-Command','Start-Sleep 60'; Set-Content -Path '{pid_file}' -Value $p.Id"
        )
    } else {
        format!("sleep 60 & echo $! > '{pid_file}'")
    };
    env.set_config(&format!("[defaults]\nexec = [\"{spawn}\"]\n"));
    let started = Instant::now();
    let out = env.wt(&["new", "feat/bg"]);
    // The grandchild sleeps 60s; wt itself only waits out DRAIN_GRACE (2s)
    // plus process-spawn overhead (two pwsh starts, git). 30s is well past
    // any of that even under heavy parallel-test contention, and still
    // half the grandchild's lifetime, so this cannot pass by accident.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the caller's pipes stayed open: {:?}",
        started.elapsed()
    );
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(
        Env::stdout(&out).trim_end().ends_with("bg"),
        "{}",
        Env::stdout(&out)
    );
    assert!(
        Env::stderr(&out).contains("wt: exec left background processes holding its output: "),
        "{}",
        Env::stderr(&out)
    );
}

// ------------------------------------------------ unregistered path on disk

fn refuses_over(env: &Env, leftover: &std::path::Path) {
    let out = env.wt(&["new", "feat/stray"]);
    assert_eq!(out.status.code(), Some(1), "{}", Env::stderr(&out));
    assert_eq!(
        Env::stderr(&out),
        format!(
            "wt: {} exists but is not a worktree; remove it or pick another name\n",
            leftover.display()
        )
    );
    assert!(Env::stdout(&out).is_empty());
    assert!(
        !env.branches().iter().any(|b| b == "feat/stray"),
        "branch orphaned"
    );
    assert_eq!(env.trees(), Vec::<String>::new());
    assert!(
        leftover.is_dir(),
        "the pre-existing directory must be left alone"
    );
}

#[test]
fn a_stray_non_empty_directory_is_refused_untouched() {
    let env = Env::new("new-stray-full");
    let leftover = env.worktrees_root().join("feat").join("stray");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("keep.txt"), "mine").unwrap();
    refuses_over(&env, &leftover);
    assert_eq!(
        std::fs::read_to_string(leftover.join("keep.txt")).unwrap(),
        "mine"
    );
}

#[test]
fn a_stray_empty_directory_is_refused_untouched() {
    let env = Env::new("new-stray-empty");
    let leftover = env.worktrees_root().join("feat").join("stray");
    std::fs::create_dir_all(&leftover).unwrap();
    refuses_over(&env, &leftover);
    assert_eq!(std::fs::read_dir(&leftover).unwrap().count(), 0);
}
