mod common;

use std::path::PathBuf;

use common::Env;

/// A loose port of the Python `BRANCH_SPEC`
/// (`^[a-z0-9]+/[a-z0-9]+(?:[-/][a-z0-9]+)*$`), just for asserting a branch
/// name shape in these black-box tests; the real check lives in
/// `domain::naming::is_branch_spec`, not reachable from a binary crate's
/// integration tests.
fn looks_like_branch_spec(text: &str) -> bool {
    let is_lower_alnum = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit();
    let Some((head, _)) = text.split_once(['-', '/']) else {
        return false;
    };
    text.strip_prefix(head)
        .is_some_and(|rest| rest.starts_with('/'))
        && text
            .split(['-', '/'])
            .all(|w| !w.is_empty() && w.chars().all(is_lower_alnum))
}

fn create(env: &Env, payload: &str) -> std::process::Output {
    env.wt_stdin(&["hook-create"], payload.as_bytes())
}

fn remove(env: &Env, payload: &str) -> std::process::Output {
    env.wt_stdin(&["hook-remove"], payload.as_bytes())
}

fn created_path(out: &std::process::Output) -> PathBuf {
    assert!(out.status.success(), "{}", Env::stderr(out));
    PathBuf::from(Env::stdout(out).trim_end())
}

// ------------------------------------------------------------- TestHooks

#[test]
fn test_two_sessions_do_not_share_a_worktree() {
    let env = Env::new("hooks-two-sessions");
    let a = created_path(&create(&env, r#"{"session_id":"aaaaaaaa-1111"}"#));
    let b = created_path(&create(&env, r#"{"session_id":"bbbbbbbb-2222"}"#));
    assert_ne!(a, b, "a second session was handed the first one's tree");
}

#[test]
fn test_the_same_session_reuses_its_worktree() {
    let env = Env::new("hooks-same-session");
    let a = created_path(&create(&env, r#"{"session_id":"aaaaaaaa-1111"}"#));
    let b = created_path(&create(&env, r#"{"session_id":"aaaaaaaa-1111"}"#));
    assert_eq!(a, b);
}

#[test]
fn test_prose_is_slugified_never_used_as_a_ref() {
    let env = Env::new("hooks-prose");
    let path = created_path(&create(
        &env,
        r#"{"name":"rydde opp i gamle feilmeldinger"}"#,
    ));
    let branch = env.git_at(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(looks_like_branch_spec(&branch), "{branch}");
}

#[test]
fn test_a_full_branch_name_is_taken_verbatim() {
    let env = Env::new("hooks-verbatim");
    let path = created_path(&create(&env, r#"{"name":"feat/xledger-error-cleanup"}"#));
    let branch = env.git_at(&path, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch, "feat/xledger-error-cleanup");
}

#[test]
fn test_a_malformed_payload_creates_nothing() {
    let env = Env::new("hooks-malformed");
    let out = create(&env, "{not json");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(Env::stderr(&out), "wt: hook payload was not JSON\n");
    assert_eq!(env.trees(), Vec::<String>::new());
}

#[test]
fn test_a_json_array_payload_is_refused_not_crashed() {
    let env = Env::new("hooks-array");
    let out = create(&env, "[]");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        Env::stderr(&out),
        "wt: hook payload was a JSON list, expected an object\n"
    );
    assert_eq!(env.trees(), Vec::<String>::new());
}

#[test]
fn test_remove_needs_the_exact_path_not_a_leaf() {
    let env = Env::new("hooks-exact-path");
    let path = created_path(&create(&env, r#"{"name":"feat/hooked"}"#));

    let leaf_only = remove(&env, r#"{"worktreePath":"feat/hooked"}"#);
    assert_eq!(leaf_only.status.code(), Some(1));
    assert!(
        path.exists(),
        "an unmatched payload must remove nothing: {}",
        Env::stderr(&leaf_only)
    );

    let full_path = format!(r#"{{"worktreePath":{:?}}}"#, path.display().to_string());
    let out = remove(&env, &full_path);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
    assert!(
        Env::stderr(&out).contains(&format!("hook-remove: removed {}", path.display())),
        "{}",
        Env::stderr(&out)
    );
}

#[test]
fn test_remove_does_not_force_over_a_dirty_tree() {
    let env = Env::new("hooks-dirty");
    let path = created_path(&create(&env, r#"{"name":"feat/dirtyhook"}"#));
    // An untracked file alone is enough to dirty `status --porcelain`.
    std::fs::write(path.join("untracked.txt"), "x").unwrap();

    let full_path = format!(r#"{{"worktreePath":{:?}}}"#, path.display().to_string());
    let out = remove(&env, &full_path);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        path.exists(),
        "unattended teardown must not destroy uncommitted work"
    );
}

// ------------------------------------------------------------------------ F8

#[cfg(windows)]
#[test]
fn f8_remove_matches_across_separators_and_case() {
    let env = Env::new("hooks-f8-case");
    let path = created_path(&create(&env, r#"{"name":"feat/casehook"}"#));
    let mangled = path.display().to_string().replace('\\', "/").to_uppercase();
    let out = remove(&env, &format!(r#"{{"worktreePath":{mangled:?}}}"#));
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}

#[test]
fn a_failed_exec_step_still_hands_claude_the_tree() {
    // Controller ruling: a non-strict exec failure must not fail the
    // WorktreeCreate hook, or Claude Code discards a tree that exists.
    let env = Env::new("hooks-exec-partial");
    env.set_config("[defaults]\nexec = [\"exit 1\"]\n");
    let out = create(&env, r#"{"name":"feat/partialhook"}"#);
    assert_eq!(out.status.code(), Some(0), "{}", Env::stderr(&out));
    let path = PathBuf::from(Env::stdout(&out).trim_end());
    assert!(path.is_dir());
    let stderr = Env::stderr(&out);
    assert!(
        stderr.contains("wt: 1 exec step(s) failed; the worktree is at"),
        "{stderr}"
    );
}

#[cfg(windows)]
#[test]
fn f8_remove_matches_a_short_name_form_of_the_path() {
    let env = Env::new("hooks-f8-short");
    let path = created_path(&create(&env, r#"{"name":"feat/shorthook"}"#));
    let short = common::short_path(&path);
    // No 8.3 names on this volume: nothing for canonicalising to resolve.
    if short == path {
        return;
    }
    let out = remove(
        &env,
        &format!(r#"{{"worktreePath":{:?}}}"#, short.display().to_string()),
    );
    assert!(out.status.success(), "{}", Env::stderr(&out));
    assert!(!path.exists());
}
