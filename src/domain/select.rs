use std::path::{Path, PathBuf};

use crate::domain::config;
use crate::domain::parse::{Meta, Worktree};
use crate::error::AppError;

/// Every worktree matching `want`: an exact branch name, the leaf directory
/// name, or a `/`-anchored suffix of the path (so `wt cd 438` cannot select
/// `fix/21438-msal-loop`).
#[must_use]
pub fn find_targets<'a>(trees: &'a [Worktree], want: &str) -> Vec<&'a Worktree> {
    trees
        .iter()
        .filter(|w| {
            w.path.file_name().and_then(|n| n.to_str()) == Some(want)
                || w.branch.as_deref() == Some(want)
                || posix_path(&w.path).ends_with(&format!("/{want}"))
        })
        .collect()
}

fn posix_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The single worktree `want` names, or a refusal. Ambiguity is reported
/// rather than resolved: `cd` and `rm` both act on the result, and guessing
/// wrong at either is expensive.
pub fn select_one<'a>(trees: &'a [Worktree], want: &str) -> Result<&'a Worktree, AppError> {
    let matches = find_targets(trees, want);
    match matches.as_slice() {
        [] => Err(AppError::NoWorktreeMatching(want.to_owned())),
        [one] => Ok(*one),
        many => Err(AppError::Ambiguous {
            want: want.to_owned(),
            count: many.len(),
            listing: many
                .iter()
                .map(|w| w.path.display().to_string())
                .collect::<Vec<_>>()
                .join("\n  "),
        }),
    }
}

/// The sidecar recorded for this worktree path, if any, out of every
/// sidecar `read_all_meta` loaded. Keyed off the path `create()` records, so
/// this never has to recompute a mode-scoped root.
#[must_use]
pub fn meta_for_path<'a>(sidecars: &'a [(PathBuf, Meta)], want: &Path) -> Option<&'a Meta> {
    sidecar_for_path(sidecars, want).map(|(_, meta)| meta)
}

/// The branch an earlier `wt item <id>` created, so a retry after a partial
/// failure lands on the same tree instead of a second branch and link (the
/// name came from a model that may answer differently now). F18: only an
/// `item` sidecar (a PR's `id` shares the key space) whose path is still a
/// registered, non-prunable worktree (not one left behind by a manual
/// removal).
#[must_use]
pub fn recorded_item_branch<'a>(
    sidecars: &'a [(PathBuf, Meta)],
    worktrees: &[Worktree],
    id: &str,
) -> Option<&'a str> {
    sidecars
        .iter()
        .map(|(_, meta)| meta)
        .filter(|m| m.mode.as_deref() == Some("item") && m.id.as_deref() == Some(id))
        .filter(|m| {
            m.path.as_deref().is_some_and(|p| {
                worktrees
                    .iter()
                    .any(|w| !w.prunable && config::same_path(&w.path, Path::new(p)))
            })
        })
        .find_map(|m| m.branch.as_deref().filter(|b| !b.is_empty()))
}

/// `meta_for_path`, with the sidecar file it came from.
#[must_use]
pub fn sidecar_for_path<'a>(
    sidecars: &'a [(PathBuf, Meta)],
    want: &Path,
) -> Option<&'a (PathBuf, Meta)> {
    sidecars.iter().find(|(_, meta)| {
        meta.path
            .as_ref()
            .is_some_and(|recorded| config::same_path(Path::new(recorded), want))
    })
}

/// The name `wt ls` shows and `wt cd`/`wt complete` accept: the worktree's
/// path relative to the mode's (provider-resolved, F19) root, in POSIX
/// form. Outside that root entirely, the bare directory name is shown
/// instead. `meta`'s recorded mode decides the root (`"new"` when there is
/// no sidecar, or no `mode` key), the one home for that default - `ls` and
/// `complete` both call this rather than each defaulting it themselves.
#[must_use]
pub fn leaf_of(
    root: &Path,
    cfg: &toml::Table,
    provider: &str,
    meta: &Meta,
    tree_path: &Path,
) -> String {
    let mode = meta.mode.as_deref().unwrap_or("new");
    let opts = config::resolve(cfg, mode, provider);
    let wt_root = config::wt_root_of(root, &opts);
    config::strip_under(tree_path, &wt_root).map_or_else(
        || {
            tree_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned()
        },
        |rel| rel.to_string_lossy().replace('\\', "/"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(path: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: PathBuf::from(path),
            branch: branch.map(str::to_owned),
            head: String::new(),
            prunable: false,
            gone: false,
        }
    }

    fn trees() -> Vec<Worktree> {
        vec![
            tree("/r.worktrees/feat/dup", Some("feat/dup")),
            tree("/r.worktrees/fix/dup", Some("fix/dup")),
            tree(
                "/r.worktrees/fix/21438-msal-loop",
                Some("fix/21438-msal-loop"),
            ),
        ]
    }

    // -------------------------------------------------------------- TestFindTargets

    #[test]
    fn test_branch_name_is_unique() {
        assert_eq!(find_targets(&trees(), "feat/dup").len(), 1);
    }

    #[test]
    fn test_shared_leaf_is_ambiguous() {
        assert_eq!(find_targets(&trees(), "dup").len(), 2);
    }

    #[test]
    fn test_suffix_is_anchored() {
        assert!(find_targets(&trees(), "loop").is_empty());
        assert!(find_targets(&trees(), "438").is_empty());
    }

    // ---------------------------------------------------------------------- select_one

    #[test]
    fn select_one_returns_the_unique_match() {
        let trees = trees();
        let w = select_one(&trees, "feat/dup").unwrap();
        assert_eq!(w.branch.as_deref(), Some("feat/dup"));
    }

    #[test]
    fn select_one_refuses_no_match() {
        assert!(matches!(
            select_one(&trees(), "nope"),
            Err(AppError::NoWorktreeMatching(_))
        ));
    }

    #[test]
    fn select_one_refuses_ambiguous_match() {
        match select_one(&trees(), "dup") {
            Err(AppError::Ambiguous { count, .. }) => assert_eq!(count, 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------------- F19

    #[test]
    fn f19_leaf_of_honours_a_provider_root_override() {
        let cfg: toml::Table = toml::from_str(
            r#"
            [mode.new]
            root = "../default.worktrees"

            [mode.new.github]
            root = "../gh.worktrees"
            "#,
        )
        .unwrap();
        let root = Path::new("/repo");
        let tree_path = Path::new("/gh.worktrees/feat/x");
        let meta = Meta {
            mode: Some("new".to_owned()),
            ..Meta::default()
        };
        assert_eq!(
            leaf_of(root, &cfg, "github", &meta, tree_path),
            "feat/x",
            "the github-scoped root must be the one leaf_of resolves against"
        );
        assert_eq!(
            leaf_of(root, &cfg, "none", &meta, tree_path),
            "x",
            "without the provider override, the tree falls outside the default root"
        );
    }

    #[test]
    fn leaf_of_defaults_to_mode_new_when_the_sidecar_has_no_mode() {
        let cfg: toml::Table = toml::from_str("[mode.new]\nroot = \"../nw.worktrees\"\n").unwrap();
        let root = Path::new("/repo");
        let tree_path = Path::new("/nw.worktrees/feat/x");
        assert_eq!(
            leaf_of(root, &cfg, "none", &Meta::default(), tree_path),
            "feat/x",
            "no sidecar (or a sidecar with no mode key) must resolve mode.new's root"
        );
    }

    #[test]
    fn leaf_of_falls_back_to_the_bare_name_outside_the_root() {
        let cfg = toml::Table::new();
        let root = Path::new("/repo");
        let tree_path = Path::new("/elsewhere/thing");
        assert_eq!(
            leaf_of(root, &cfg, "none", &Meta::default(), tree_path),
            "thing"
        );
    }

    // ------------------------------------------------ TestItemPublishing / F18

    fn item_meta(mode: &str, id: &str, path: &str, branch: Option<&str>) -> (PathBuf, Meta) {
        (
            PathBuf::from("x.json"),
            Meta {
                mode: Some(mode.to_owned()),
                id: Some(id.to_owned()),
                path: Some(path.to_owned()),
                branch: branch.map(str::to_owned),
                ..Meta::default()
            },
        )
    }

    #[test]
    fn test_a_retry_reuses_the_recorded_branch() {
        let path = "/r.worktrees/fix/21438-msal-loop";
        let sidecars = [item_meta(
            "item",
            "21438",
            path,
            Some("fix/21438-msal-loop"),
        )];
        assert_eq!(
            recorded_item_branch(&sidecars, &trees(), "21438"),
            Some("fix/21438-msal-loop")
        );
        assert_eq!(recorded_item_branch(&sidecars, &trees(), "2143"), None);
    }

    #[test]
    fn f18_only_a_registered_item_sidecar_is_trusted() {
        let path = "/r.worktrees/fix/21438-msal-loop";
        // #34: a PR sidecar shares the id key space.
        let pr = [item_meta("pr", "21438", path, Some("fix/21438-msal-loop"))];
        assert_eq!(recorded_item_branch(&pr, &trees(), "21438"), None);
        // #33: left behind after a manual `git worktree remove`.
        let gone = [item_meta(
            "item",
            "21438",
            "/r.worktrees/gone",
            Some("fix/gone"),
        )];
        assert_eq!(recorded_item_branch(&gone, &trees(), "21438"), None);
        let detached = [item_meta("item", "21438", path, None)];
        assert_eq!(recorded_item_branch(&detached, &trees(), "21438"), None);
        // Still registered, but git reports it prunable (deleted by hand).
        let sidecars = [item_meta(
            "item",
            "21438",
            path,
            Some("fix/21438-msal-loop"),
        )];
        let prunable: Vec<Worktree> = trees()
            .into_iter()
            .map(|w| Worktree {
                prunable: true,
                ..w
            })
            .collect();
        assert_eq!(recorded_item_branch(&sidecars, &prunable, "21438"), None);
    }
}
