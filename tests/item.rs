// Marks this file as test code, so clippy.toml allows unwrap in its helpers too.
#![cfg(test)]

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::Env;

const BRANCH: &str = "fix/21438-msal-login-loop";

/// A `wt` run with `az`, `gh`, `claude` and `herdr` all hidden from PATH, so
/// a test that passes proves no tracker call was made. `Env::hidden_path`
/// keeps git (and the shell it needs) reachable even when a hidden tool
/// shares its directory, as `gh` does with `git` in `/usr/bin` on GitHub's
/// ubuntu runner.
fn wt_no_tracker(env: &Env, args: &[&str]) -> Output {
    let path = env.hidden_path(&["az", "gh", "claude", "herdr"]);
    env.wt_at_with_path(&env.repo(), args, path.to_str().unwrap())
}

/// The tree an earlier `wt item 21438` left behind, with its sidecar.
fn earlier_item_tree(env: &Env, id: &str, mode: &str) -> PathBuf {
    let path = env
        .worktrees_root()
        .join("fix")
        .join("21438-msal-login-loop");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    env.git(&[
        "worktree",
        "add",
        "-b",
        BRANCH,
        path.to_str().unwrap(),
        "main",
    ]);
    write_item_sidecar(env, &path, id, mode);
    path
}

fn write_item_sidecar(env: &Env, path: &Path, id: &str, mode: &str) {
    let meta = serde_json::json!({
        "mode": mode,
        "path": path.display().to_string(),
        "branch": BRANCH,
        "base": "origin/main",
        "detached": false,
        "copied": [],
        "created": "2026-01-01T00:00:00+00:00",
        "title": "MSAL login loop",
        "id": id,
    });
    let dir = env.repo().join(".git").join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("fix%2F21438-msal-login-loop.json"),
        serde_json::to_string_pretty(&meta).unwrap(),
    )
    .unwrap();
}

/// Origin looks like GitHub to wt, but pushes land in the local bare repo.
fn github_shaped_origin(env: &Env) {
    let bare = env.root.join("origin.git");
    env.git(&["remote", "set-url", "origin", "https://github.com/o/r.git"]);
    env.git(&[
        "remote",
        "set-url",
        "--push",
        "origin",
        bare.to_str().unwrap(),
    ]);
}

fn on_origin(env: &Env, branch: &str) -> bool {
    let bare = env.root.join("origin.git");
    env.command("git", &env.root)
        .args([
            "--git-dir",
            bare.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
        ])
        .arg(format!("refs/heads/{branch}"))
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn item_without_a_supported_provider_is_refused() {
    let env = Env::new("item-none");
    let out = env.wt(&["item", "21438"]);
    assert!(!out.status.success());
    assert_eq!(
        Env::stderr(&out),
        "wt: no supported provider on origin; cannot resolve a work item id\n"
    );
    assert_eq!(env.trees(), Vec::<String>::new());
    assert_eq!(Env::stdout(&out), "");
}

#[test]
fn run_without_open_is_refused_on_item() {
    let env = Env::new("item-run");
    let out = env.wt(&["item", "1", "--run", "x"]);
    assert!(!out.status.success());
    assert_eq!(
        Env::stderr(&out),
        "wt: --run has nowhere to run without --open\n"
    );
}

#[test]
fn test_a_retry_reuses_the_recorded_branch() {
    let env = Env::new("item-reuse-github");
    github_shaped_origin(&env);
    env.set_config("[mode.item]\nlink_branch = true\nset_state = \"Active\"\n");
    let path = earlier_item_tree(&env, "21438", "item");

    let out = wt_no_tracker(&env, &["item", "21438", "--slug", "totally-different-name"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert_eq!(
        Env::stderr(&out),
        format!(
            "  21438: reusing {BRANCH} from an earlier run\n\
             exists: {}\n  pushed {BRANCH}\n\
             \x20 link_branch: no github equivalent, skipped\n\
             \x20 set_state: no github equivalent, skipped\n",
            path.display()
        )
    );
    assert!(on_origin(&env, BRANCH), "the branch was not pushed");
    assert_eq!(env.trees().len(), 1, "a second tree was made");
}

#[test]
fn link_and_state_are_not_attempted_unless_configured() {
    let env = Env::new("item-reuse-defaults");
    env.set_config("[mode.item]\nset_state = \"\"\n");
    // On a GitHub origin an attempted link or state is reported as a skip,
    // so its absence from the (non -q) chatter proves it was not attempted.
    github_shaped_origin(&env);
    earlier_item_tree(&env, "21438", "item");

    let out = wt_no_tracker(&env, &["item", "21438"]);
    let stderr = Env::stderr(&out);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains(&format!("  pushed {BRANCH}")), "{stderr}");
    assert!(!stderr.contains("link_branch"), "{stderr}");
    assert!(!stderr.contains("set_state"), "{stderr}");
    assert!(on_origin(&env, BRANCH));
}

#[test]
fn test_a_failed_push_tells_the_tracker_nothing() {
    let env = Env::new("item-push-fails");
    env.set_config("[mode.item]\nlink_branch = true\nset_state = \"Active\"\n");
    let path = earlier_item_tree(&env, "21438", "item");
    let nowhere = env.root.join("no-such-origin.git");
    env.git(&[
        "remote",
        "set-url",
        "--push",
        "origin",
        nowhere.to_str().unwrap(),
    ]);

    let out = wt_no_tracker(&env, &["-q", "item", "21438"]);
    assert_eq!(out.status.code(), Some(1));
    // The partial-failure protocol: the path still goes out, so the
    // wrapper cds into the tree that does exist.
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    let stderr = Env::stderr(&out);
    // git's own message runs over several lines.
    let lines: Vec<&str> = stderr.lines().collect();
    assert!(lines[0].starts_with("  push failed: "), "{stderr}");
    assert!(!stderr.contains("linked"), "{stderr}");
    assert_eq!(
        lines[lines.len() - 2..],
        [
            "  skipped link/state: branch is not on origin".to_owned(),
            format!(
                "wt: 2 post-create step(s) failed; the worktree is at {}",
                path.display()
            ),
        ]
    );
}

#[test]
fn f18_a_pr_sidecar_with_the_same_id_is_not_reused() {
    let env = Env::new("item-pr-sidecar");
    earlier_item_tree(&env, "21438", "pr");
    let out = wt_no_tracker(&env, &["item", "21438"]);
    assert!(!out.status.success());
    assert_eq!(
        Env::stderr(&out),
        "wt: no supported provider on origin; cannot resolve a work item id\n"
    );
}

#[test]
fn f18_a_sidecar_whose_tree_is_gone_is_not_reused() {
    let env = Env::new("item-gone");
    let path = earlier_item_tree(&env, "21438", "item");
    env.git(&["worktree", "remove", "--force", path.to_str().unwrap()]);
    let out = wt_no_tracker(&env, &["item", "21438"]);
    assert!(!out.status.success());
    assert_eq!(
        Env::stderr(&out),
        "wt: no supported provider on origin; cannot resolve a work item id\n"
    );
}

#[test]
fn a_github_item_without_gh_says_so() {
    let env = Env::new("item-no-gh");
    github_shaped_origin(&env);
    let out = wt_no_tracker(&env, &["item", "7"]);
    assert!(!out.status.success());
    assert_eq!(Env::stderr(&out), "wt: gh not found on PATH\n");
    assert_eq!(env.trees(), Vec::<String>::new());
}

#[test]
fn test_a_missing_herdr_is_not_a_failure() {
    let env = Env::new("item-no-herdr");
    env.set_config("[mode.item]\nset_state = \"\"\n");
    let path = earlier_item_tree(&env, "21438", "item");

    let out = wt_no_tracker(&env, &["item", "21438", "--open", "--run", "claude"]);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert_eq!(Env::stdout(&out), format!("{}\n", path.display()));
    assert!(
        Env::stderr(&out).ends_with(&format!(
            "  pushed {BRANCH}\n  herdr not on PATH, skipped\n"
        )),
        "{}",
        Env::stderr(&out)
    );
}
