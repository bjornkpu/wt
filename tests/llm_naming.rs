mod common;

use std::path::Path;
use std::process::Command;

use common::Env;

// wt never spawns claude in a test: this pins PATH to a directory with only
// git in it (found the way a shell would - `where`/`which git`), so the real
// PATH lookup inside wt fails on its own, without ever needing `claude`
// installed to prove the fallback works.
#[test]
fn no_claude_on_path_falls_back_and_still_creates_the_tree() {
    let env = Env::new("llm-no-claude");
    std::fs::write(
        env.root.join("wt-home/config.toml"),
        "[naming]\nllm = true\n",
    )
    .unwrap();

    let finder = if cfg!(windows) { "where" } else { "which" };
    let found = Command::new(finder).arg("git").output().unwrap();
    assert!(found.status.success(), "{finder} git failed to find git");
    let stdout = String::from_utf8(found.stdout).unwrap();
    let git_path = stdout.lines().next().unwrap();
    let git_only_path = Path::new(git_path).parent().unwrap().display().to_string();

    let out = env.wt_at_with_path(&env.repo(), &["new", "some prose here"], &git_only_path);
    assert!(out.status.success(), "{}", Env::stderr(&out));
    let stderr = Env::stderr(&out);
    assert!(stderr.contains("PATH"), "{stderr}");
    assert!(
        Env::stdout(&out).trim_end().ends_with("some-prose-here"),
        "{}",
        Env::stdout(&out)
    );
    assert!(env.branches().contains(&"feat/some-prose-here".to_owned()));
}
