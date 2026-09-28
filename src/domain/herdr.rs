//! herdr: argv builders, reply parsers, the open outcome texts and the
//! `release_panes` decision. The IO that runs them is in `io/cli.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::domain::config;
use crate::domain::naming;
use crate::domain::parse::{Meta, Worktree};
use crate::domain::provider;
use crate::error::{AppError, python_repr};

/// How an open ended. A missing herdr is a skip, not a failure: the tree was
/// made as asked, there is just no workspace to put it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok(String),
    Skipped(String),
    Failed(String),
}

impl Outcome {
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Ok(d) | Self::Skipped(d) | Self::Failed(d) => d,
        }
    }
}

pub const SKIPPED: &str = "herdr not on PATH, skipped";

/// The workspace label for `mode`: `[herdr.label] <mode>`, else
/// `label_default`, else `{repo}`. Every placeholder a label may use is
/// supplied, empty when the mode has none, so a `label_default` naming
/// `{id}` still works for a mode without one.
pub fn label_for(
    cfg: &toml::Table,
    mode: &str,
    repo: &str,
    id: &str,
    branch: &str,
) -> Result<String, AppError> {
    let herdr = cfg.get("herdr").and_then(toml::Value::as_table);
    let by_mode = herdr
        .and_then(|h| h.get("label"))
        .and_then(toml::Value::as_table)
        .and_then(|l| l.get(mode))
        .and_then(toml::Value::as_str)
        .filter(|t| !t.is_empty());
    // A fill() template, not a format string.
    #[allow(clippy::literal_string_with_formatting_args)]
    let template = by_mode
        .or_else(|| herdr?.get("label_default")?.as_str())
        .unwrap_or("{repo}");
    naming::fill(template, &[("repo", repo), ("id", id), ("branch", branch)])
}

/// `cmds` then `--run`. An empty `--run` is absent, as Python's falsy check
/// had it: not sent, not counted.
#[must_use]
pub fn with_run(mut cmds: Vec<String>, run: Option<&str>) -> Vec<String> {
    cmds.extend(run.filter(|r| !r.is_empty()).map(str::to_owned));
    cmds
}

/// What `wt open` hands the workspace for an existing tree.
#[derive(Debug, PartialEq, Eq)]
pub struct Reopen {
    pub mode: String,
    pub id: String,
    pub branch: String,
    pub cmds: Vec<String>,
}

/// The mode the sidecar recorded (`new` without one), so a reopened tree
/// keeps the label its create gave it; the id; git's branch, else the
/// sidecar's; and `--run`.
#[must_use]
pub fn reopen(meta: &Meta, tree: &Worktree, run: Option<&str>) -> Reopen {
    Reopen {
        mode: meta.mode.clone().unwrap_or_else(|| "new".to_owned()),
        id: meta.id.clone().unwrap_or_default(),
        branch: tree
            .branch
            .clone()
            .or_else(|| meta.branch.clone())
            .unwrap_or_default(),
        cmds: with_run(Vec::new(), run),
    }
}

fn owned(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_owned()).collect()
}

fn native(path: &Path) -> String {
    config::native(path).display().to_string()
}

#[must_use]
pub fn worktree_list_argv(root: &Path) -> Vec<String> {
    owned(&["herdr", "worktree", "list", "--cwd", &native(root)])
}

/// `--workspace` always: left to itself herdr either refuses from a
/// worktree's own workspace or retargets the repo's workspace into the new
/// tree. `--no-focus`: the commands run over there while you stay here.
#[must_use]
pub fn worktree_open_argv(source: &str, path: &Path, label: &str) -> Vec<String> {
    let path = native(path);
    let mut argv = owned(&[
        "herdr",
        "worktree",
        "open",
        "--workspace",
        source,
        "--path",
        &path,
        "--no-focus",
    ]);
    if !label.is_empty() {
        argv.extend(owned(&["--label", label]));
    }
    argv
}

#[must_use]
pub fn pane_run_argv(pane: &str, cmd: &str) -> Vec<String> {
    owned(&["herdr", "pane", "run", pane, cmd])
}

#[must_use]
pub fn pane_list_argv() -> Vec<String> {
    owned(&["herdr", "pane", "list"])
}

/// Whether herdr's `child` path is `parent` or under it. herdr reports
/// paths with its own separators, so backslashes are unified first.
fn same_or_under(parent: &Path, child: &str) -> bool {
    config::strip_under(Path::new(&child.replace('\\', "/")), parent).is_some()
}

/// The workspace holding the parent checkout, out of `worktree list`: the
/// first tree at or under `root` that has one open.
#[must_use]
pub fn parent_workspace(root: &Path, reply: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(reply).ok()?;
    reply
        .pointer("/result/worktrees")?
        .as_array()?
        .iter()
        .filter(|t| {
            same_or_under(
                root,
                t.get("path").and_then(Value::as_str).unwrap_or_default(),
            )
        })
        .find_map(|t| {
            t.get("open_workspace_id")?
                .as_str()
                .filter(|id| !id.is_empty())
        })
        .map(str::to_owned)
}

/// The new workspace's root pane, out of the `worktree open` reply.
#[must_use]
pub fn pane_of(reply: &str) -> Option<String> {
    let reply: Value = serde_json::from_str(reply).ok()?;
    reply
        .pointer("/result/root_pane/pane_id")?
        .as_str()
        .map(str::to_owned)
}

/// One entry of `pane list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pane {
    pub id: String,
    pub cwd: String,
    pub agent: String,
}

/// `pane list`'s panes, or none if the reply cannot say.
#[must_use]
pub fn panes(reply: &str) -> Vec<Pane> {
    let Ok(reply) = serde_json::from_str::<Value>(reply) else {
        return Vec::new();
    };
    let text = |p: &Value, key: &str| {
        p.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    reply
        .pointer("/result/panes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|p| Pane {
            id: text(p, "pane_id"),
            cwd: text(p, "cwd"),
            agent: text(p, "agent"),
        })
        .collect()
}

#[must_use]
pub fn no_workspace(root: &Path) -> String {
    format!(
        "herdr: no workspace open on {}; open one there first",
        native(root)
    )
}

fn excerpt(err: &str, out: &str) -> String {
    let text = if err.trim().is_empty() { out } else { err };
    provider::excerpt(text)
}

#[must_use]
pub fn open_failed(err: &str, out: &str) -> String {
    format!("herdr failed: {}", excerpt(err, out))
}

/// What an open says once the workspace exists.
#[must_use]
pub fn opened(label: &str, path: &Path) -> String {
    if label.is_empty() {
        format!("herdr: {}", native(path))
    } else {
        format!("herdr: {label}")
    }
}

#[must_use]
pub fn no_pane(opened: &str, count: usize) -> String {
    format!("{opened}, but the reply named no pane to run {count} command(s) in")
}

#[must_use]
pub fn send_failed(opened: &str, cmd: &str, err: &str, out: &str) -> String {
    format!(
        "{opened}, but sending {} failed: {}",
        python_repr(cmd),
        excerpt(err, out)
    )
}

#[must_use]
pub fn running(opened: &str, count: usize) -> String {
    format!("{opened}, running {count} command(s) there")
}

/// What `release_panes` does: the `pane run` calls to make, in order, and the
/// panes in the tree it will not type into, for a failed removal's message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Release {
    pub send: Vec<Vec<String>>,
    pub left: Vec<String>,
    /// F22: set when a `cd` to `root` cannot be typed safely.
    pub warning: Option<String>,
}

/// Which panes sitting in `path` get a `cd` back to `root`. Never the pane
/// wt runs in (`me`, from `HERDR_PANE_ID`): a `cd` there waits in its input
/// until wt exits. Never an agent pane: a `cd` typed at claude is a prompt.
/// F22: never at all when `root` holds `"`, `$` or a backtick, which the
/// pane's shell would interpret.
#[must_use]
pub fn release_panes(root: &Path, path: &Path, panes: &[Pane], me: &str) -> Release {
    let mut left = Vec::new();
    let mut moving = BTreeSet::new();
    for p in panes.iter().filter(|p| same_or_under(path, &p.cwd)) {
        if p.id == me {
            left.push(format!("{} (the shell wt is running in)", p.id));
        } else if !p.agent.is_empty() {
            left.push(format!("{} (running {})", p.id, p.agent));
        } else {
            moving.insert(p.id.as_str());
        }
    }
    let root = native(root);
    if moving.is_empty() || !root.contains(['"', '$', '`']) {
        let cd = format!("cd \"{root}\"");
        let send = moving.iter().map(|id| pane_run_argv(id, &cd)).collect();
        return Release {
            send,
            left,
            warning: None,
        };
    }
    let ids: Vec<&str> = moving.into_iter().collect();
    left.extend(
        ids.iter()
            .map(|id| format!("{id} (not sent a cd: unsafe path)")),
    );
    let warning = format!(
        "wt: not typing a cd into herdr pane(s) {}: {root} holds a quote, $ or backtick its shell would interpret",
        ids.join(", ")
    );
    Release {
        send: Vec::new(),
        left,
        warning: Some(warning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(text: &str) -> toml::Table {
        toml::from_str(text).unwrap()
    }

    // ----------------------------------------------------------------- label_for

    #[test]
    fn label_prefers_the_mode_then_the_default_then_the_repo() {
        let both = cfg(
            "[herdr]\nlabel_default = \"{repo} · {branch}\"\n[herdr.label]\npr = \"{repo} · PR {id}\"\n",
        );
        assert_eq!(
            label_for(&both, "pr", "mimir", "7", "feat/x").unwrap(),
            "mimir · PR 7"
        );
        assert_eq!(
            label_for(&both, "new", "mimir", "", "feat/x").unwrap(),
            "mimir · feat/x"
        );
        assert_eq!(
            label_for(&toml::Table::new(), "new", "mimir", "", "feat/x").unwrap(),
            "mimir"
        );
    }

    #[test]
    fn an_empty_mode_label_falls_back_and_a_missing_placeholder_is_empty() {
        let c = cfg("[herdr]\nlabel_default = \"{repo}#{id}\"\n[herdr.label]\nnew = \"\"\n");
        assert_eq!(label_for(&c, "new", "mimir", "", "b").unwrap(), "mimir#");
    }

    #[test]
    fn an_unknown_label_placeholder_is_an_error() {
        let c = cfg("[herdr]\nlabel_default = \"{slug}\"\n");
        assert!(label_for(&c, "new", "mimir", "", "").is_err());
    }

    // -------------------------------------------------------------------- reopen

    fn tree(branch: Option<&str>) -> Worktree {
        Worktree {
            path: "C:/t".into(),
            branch: branch.map(str::to_owned),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        }
    }

    #[test]
    fn reopen_takes_mode_and_id_from_the_sidecar_and_the_branch_from_git() {
        let meta = Meta {
            mode: Some("pr".to_owned()),
            id: Some("7".to_owned()),
            branch: Some("old".to_owned()),
            ..Meta::default()
        };
        assert_eq!(
            reopen(&meta, &tree(Some("feat/target")), Some("claude")),
            Reopen {
                mode: "pr".to_owned(),
                id: "7".to_owned(),
                branch: "feat/target".to_owned(),
                cmds: vec!["claude".to_owned()],
            }
        );
    }

    #[test]
    fn reopen_defaults_to_new_and_falls_back_to_the_sidecar_branch() {
        let bare = reopen(&Meta::default(), &tree(None), None);
        assert_eq!(
            bare,
            Reopen {
                mode: "new".to_owned(),
                id: String::new(),
                branch: String::new(),
                cmds: Vec::new(),
            }
        );
        let meta = Meta {
            branch: Some("feat/x".to_owned()),
            ..Meta::default()
        };
        assert_eq!(reopen(&meta, &tree(None), None).branch, "feat/x");
    }

    #[test]
    fn an_empty_run_is_absent() {
        let exec = vec!["uv sync".to_owned()];
        assert_eq!(with_run(exec.clone(), Some("")), exec);
        assert_eq!(with_run(Vec::new(), Some("")), Vec::<String>::new());
        assert_eq!(with_run(exec, Some("claude")), ["uv sync", "claude"]);
    }

    // ---------------------------------------------------------------------- argv

    #[test]
    fn argv_builders_match_the_python_calls() {
        let root = Path::new("C:/repos/mimir");
        assert_eq!(
            worktree_list_argv(root),
            ["herdr", "worktree", "list", "--cwd", &native(root)]
        );
        assert_eq!(
            pane_run_argv("wZ:p1", "claude"),
            ["herdr", "pane", "run", "wZ:p1", "claude"]
        );
        assert_eq!(pane_list_argv(), ["herdr", "pane", "list"]);
    }

    // ---------------------------------------------------------------- TestHerdrOpen

    #[test]
    fn test_open_names_the_parents_workspace_as_the_source() {
        let root = Path::new("C:/repos/mimir");
        let listed =
            r#"{"result":{"worktrees":[{"path":"C:\\repos\\mimir","open_workspace_id":"w1"}]}}"#;
        let source = parent_workspace(root, listed).unwrap();
        let argv = worktree_open_argv(&source, Path::new("C:/repos/mimir.worktrees/feat/x"), "l");
        let at = argv.iter().position(|a| a == "--workspace").unwrap();
        assert_eq!(argv[at + 1], "w1");
    }

    #[test]
    fn test_a_repo_without_a_workspace_is_refused_not_guessed() {
        let root = Path::new("C:/repos/mimir");
        for reply in [
            r#"{"result":{"worktrees":[{"path":"C:\\repos\\mimir","open_workspace_id":null}]}}"#,
            r#"{"result":{"worktrees":[{"path":"C:\\repos\\other","open_workspace_id":"w9"}]}}"#,
            r#"{"result":{"worktrees":[{"path":"C:\\repos\\mimir-2","open_workspace_id":"w9"}]}}"#,
            r#"{"result":{}}"#,
            "not json",
        ] {
            assert_eq!(parent_workspace(root, reply), None, "{reply}");
        }
        assert_eq!(
            no_workspace(root),
            format!(
                "herdr: no workspace open on {}; open one there first",
                native(root)
            )
        );
    }

    #[test]
    fn the_first_tree_at_or_under_the_root_with_a_workspace_wins() {
        let root = Path::new("C:/repos/mimir");
        let listed = r#"{"result":{"worktrees":[
            {"path":"C:\\repos\\mimir","open_workspace_id":""},
            {"path":"c:/Repos/Mimir/sub","open_workspace_id":"w2"},
            {"path":"C:\\repos\\mimir","open_workspace_id":"w3"}]}}"#;
        // Windows paths are case-insensitive; elsewhere the second is not
        // under the root.
        let want = if cfg!(windows) { "w2" } else { "w3" };
        assert_eq!(parent_workspace(root, listed).as_deref(), Some(want));
    }

    #[test]
    fn test_open_does_not_steal_focus() {
        let tree = Path::new("C:/t");
        let argv = worktree_open_argv("w1", tree, "mimir · feat/x");
        assert!(!argv.contains(&"--focus".to_owned()));
        assert_eq!(
            argv,
            [
                "herdr",
                "worktree",
                "open",
                "--workspace",
                "w1",
                "--path",
                &native(tree),
                "--no-focus",
                "--label",
                "mimir · feat/x"
            ]
        );
        assert!(!worktree_open_argv("w1", tree, "").contains(&"--label".to_owned()));
    }

    #[test]
    fn pane_of_reads_the_root_pane() {
        assert_eq!(
            pane_of(r#"{"result":{"root_pane":{"pane_id":"wZ:p1"}}}"#).as_deref(),
            Some("wZ:p1")
        );
        assert_eq!(pane_of("{}"), None);
        assert_eq!(pane_of("nope"), None);
    }

    #[test]
    fn outcome_texts_match_the_python_tool() {
        let path = Path::new("C:/t");
        let with = opened("mimir · feat/x", path);
        assert_eq!(with, "herdr: mimir · feat/x");
        assert_eq!(opened("", path), format!("herdr: {}", native(path)));
        assert_eq!(open_failed("  boom \n", "out"), "herdr failed: boom");
        assert_eq!(open_failed("", " out "), "herdr failed: out");
        assert_eq!(
            no_pane(&with, 2),
            "herdr: mimir · feat/x, but the reply named no pane to run 2 command(s) in"
        );
        assert_eq!(
            send_failed(&with, "pytest -q", "gone", ""),
            "herdr: mimir · feat/x, but sending 'pytest -q' failed: gone"
        );
        assert_eq!(
            running(&with, 2),
            "herdr: mimir · feat/x, running 2 command(s) there"
        );
    }

    // ------------------------------------------------------------ TestReleasePanes

    const ROOT: &str = "C:/repos/mimir";
    const TREE: &str = "C:/repos/mimir.worktrees/feat/x";

    fn pane(id: &str, cwd: &str, agent: &str) -> Pane {
        Pane {
            id: id.to_owned(),
            cwd: cwd.to_owned(),
            agent: agent.to_owned(),
        }
    }

    fn cd_root() -> String {
        format!("cd \"{}\"", native(Path::new(ROOT)))
    }

    #[test]
    fn panes_parses_the_list_reply() {
        let reply = r#"{"result":{"panes":[{"pane_id":"wS:p1","cwd":"C:\\x","agent":"claude"},{"pane_id":"wS:p2"}]}}"#;
        assert_eq!(
            panes(reply),
            [pane("wS:p1", "C:\\x", "claude"), pane("wS:p2", "", "")]
        );
        assert_eq!(panes("{}"), []);
    }

    #[test]
    fn test_a_shell_pane_is_sent_back_to_the_parent_checkout() {
        let r = release_panes(
            Path::new(ROOT),
            Path::new(TREE),
            &[pane("wS:p1", "C:\\repos\\mimir.worktrees\\feat\\x", "")],
            "",
        );
        assert_eq!(r.send, [pane_run_argv("wS:p1", &cd_root())]);
        assert_eq!(r.left, Vec::<String>::new());
    }

    #[test]
    fn test_a_pane_outside_the_tree_is_left_alone() {
        // The sibling shares a prefix up to the leaf: a plain startswith
        // would move it too.
        let r = release_panes(
            Path::new(ROOT),
            Path::new(TREE),
            &[pane(
                "wP:p1",
                "C:\\repos\\mimir.worktrees\\feat\\xylophone",
                "",
            )],
            "",
        );
        assert_eq!(r, Release::default());
    }

    #[test]
    fn test_wts_own_pane_is_never_typed_into() {
        let r = release_panes(
            Path::new(ROOT),
            Path::new(TREE),
            &[pane("wS:p1", "C:\\repos\\mimir.worktrees\\feat\\x", "")],
            "wS:p1",
        );
        assert_eq!(r.send, Vec::<Vec<String>>::new());
        assert_eq!(r.left, ["wS:p1 (the shell wt is running in)"]);
    }

    #[test]
    fn test_an_agent_pane_is_reported_not_typed_into() {
        let r = release_panes(
            Path::new(ROOT),
            Path::new(TREE),
            &[pane(
                "wQ:p1",
                "C:\\repos\\mimir.worktrees\\feat\\x\\src",
                "claude",
            )],
            "",
        );
        assert_eq!(r.send, Vec::<Vec<String>>::new());
        assert_eq!(r.left, ["wQ:p1 (running claude)"]);
    }

    #[test]
    fn panes_are_sent_the_cd_once_each_in_sorted_order() {
        let cwd = "C:/repos/mimir.worktrees/feat/x";
        let r = release_panes(
            Path::new(ROOT),
            Path::new(TREE),
            &[
                pane("w:b", cwd, ""),
                pane("w:a", cwd, ""),
                pane("w:b", cwd, ""),
            ],
            "",
        );
        assert_eq!(
            r.send,
            [
                pane_run_argv("w:a", &cd_root()),
                pane_run_argv("w:b", &cd_root())
            ]
        );
    }

    #[test]
    fn f22_a_root_the_shell_would_interpret_is_never_typed() {
        for root in ["C:/repos/a\"b", "C:/repos/a$b", "C:/repos/a`b"] {
            let tree = format!("{root}.worktrees/feat/x");
            let r = release_panes(
                Path::new(root),
                Path::new(&tree),
                &[pane("wS:p1", &tree, "")],
                "",
            );
            assert_eq!(r.send, Vec::<Vec<String>>::new(), "{root}");
            assert_eq!(r.left, ["wS:p1 (not sent a cd: unsafe path)"], "{root}");
            let warning = r.warning.unwrap();
            assert!(
                warning.contains("wS:p1") && warning.contains("backtick"),
                "{warning}"
            );
        }
        let nobody = release_panes(
            Path::new("C:/repos/a$b"),
            Path::new("C:/elsewhere"),
            &[],
            "",
        );
        assert_eq!(
            nobody.warning, None,
            "no pane to type into, nothing to report"
        );
    }
}
