use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::domain::config::{self, Options, Teardown};
use crate::domain::naming;
use crate::domain::parse::{self, BranchHistory, Meta, Worktree};
use crate::domain::provider;
use crate::domain::select;
use crate::domain::view;
use crate::error::AppError;

/// What a plan needs to know about the repo, gathered once by `run.rs`.
/// The remote's default branch is not here: it can cost a network round
/// trip, so only the verbs that build a base look it up (after their fetch).
#[derive(Debug, Clone)]
pub struct Facts {
    pub main_root: PathBuf,
    pub meta_dir: PathBuf,
    /// Where `wt` reads its config, and the naming model's cwd (F: a
    /// repo's own `CLAUDE.md` must not load into it).
    pub config_dir: PathBuf,
    pub cfg: toml::Table,
    pub provider: &'static str,
    pub remote: String,
    pub worktrees: Vec<Worktree>,
    pub sidecars: Vec<(PathBuf, Meta)>,
    /// Local branch names (`refs/heads/` stripped).
    pub branches: Vec<String>,
    pub now: jiff::Timestamp,
}

/// Everything `plan_create` needs to know about the tree to make, named.
#[derive(Debug, Clone, Default)]
pub struct CreateSpec {
    pub mode: String,
    pub branch: Option<String>,
    pub base: String,
    pub detach: bool,
    pub track: bool,
    pub dirname: Option<String>,
    pub item_id: Option<String>,
    /// `pr`: the branch the PR came from, recorded in the sidecar.
    pub pr_branch: Option<String>,
    /// `--open`: the exec steps belong to the new workspace's shell.
    pub defer_exec: bool,
    /// Who asked for this tree, recorded in the sidecar's `via` (hook-create
    /// writes `"claude"`; the CLI leaves it unset).
    pub via: Option<String>,
    /// `item`: the tracker title, recorded in the sidecar (empty on reuse).
    pub title: Option<String>,
}

/// How `git worktree add` attaches the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddArm {
    Detach { base: String },
    Existing { branch: String },
    GuessRemote { branch: String },
    New { branch: String, base: String },
}

impl AddArm {
    /// The `git worktree add` arguments for `path`.
    #[must_use]
    pub fn git_args(&self, path: &str) -> Vec<String> {
        let args: Vec<&str> = match self {
            Self::Detach { base } => vec!["worktree", "add", "--detach", path, base],
            Self::Existing { branch } => vec!["worktree", "add", path, branch],
            Self::GuessRemote { branch } => vec!["worktree", "add", "--guess-remote", path, branch],
            Self::New { branch, base } => vec!["worktree", "add", "-b", branch, path, base],
        };
        args.into_iter().map(str::to_owned).collect()
    }

    /// The branch this arm creates, which a rollback must delete again:
    /// `git worktree remove` leaves it behind.
    #[must_use]
    pub fn created_branch(&self) -> Option<&str> {
        match self {
            Self::GuessRemote { branch } | Self::New { branch, .. } => Some(branch),
            Self::Detach { .. } | Self::Existing { .. } => None,
        }
    }
}

/// One thing a verb does, as plain data. `run.rs` executes them in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// The tree is already there: hand it back.
    Reuse {
        path: PathBuf,
    },
    AddWorktree {
        path: PathBuf,
        arm: AddArm,
    },
    Seed {
        from: PathBuf,
        to: PathBuf,
        copy: Vec<String>,
        symlink: Vec<String>,
    },
    /// `meta.copied` is filled in by the executor from what `Seed` actually
    /// copied: which sources exist is only known then.
    WriteSidecar {
        file: PathBuf,
        meta: Meta,
    },
    Exec {
        cmds: Vec<String>,
        /// `None`: the platform default shell.
        shell: Option<Vec<String>>,
        timeout: Duration,
        strict: bool,
    },
    /// `--open`: the steps run in the new workspace, not here (`exec_cmds`
    /// fills them for it). Only `exec_strict` is left to report.
    DeferExec {
        strict: bool,
    },
}

/// The steps that make `spec`'s worktree, or why it must not be made.
/// Refuses a path outside the mode's root, hands back a tree that is
/// already registered there (unless it is a detached tree pinned to another
/// base), and refuses a branch another tree has checked out. Exec templates
/// are filled here, so a typo in one fails before anything is made.
/// `target_exists` is the one filesystem fact: whether `target_path` is
/// already on disk. An unregistered path there is refused before `git
/// worktree add`, which would otherwise create the branch and then fail, or
/// adopt an empty directory a rollback would then delete.
pub fn plan_create(
    facts: &Facts,
    spec: &CreateSpec,
    conf: &Options,
    target_exists: bool,
) -> Result<Vec<Step>, AppError> {
    let (leaf, path) = leaf_and_path(facts, spec, conf)?;
    if let Some(existing) = facts
        .worktrees
        .iter()
        .find(|w| config::same_path(&w.path, &path))
    {
        // A prunable match at this same path is not there to reuse: git
        // still holds it (and its branch, if any) checked out, whether its
        // directory is actually gone or its `.git` pointer alone was
        // deleted. A detached tree (pr/review shapes) has no branch to
        // name, so it is reported the way `describe_removal` already does.
        if existing.prunable {
            return Err(AppError::BranchCheckedOut {
                branch: existing
                    .branch
                    .clone()
                    .unwrap_or_else(|| "(detached)".to_owned()),
                path: config::native(&existing.path),
                hint: prunable_hint(existing),
            });
        }
        let pinned = select::meta_for_path(&facts.sidecars, &path)
            .and_then(|m| m.base.as_deref())
            .filter(|old| spec.detach && is_sha(old) && *old != spec.base);
        if let Some(old) = pinned {
            return Err(AppError::Pinned {
                path,
                old: view::short_sha(old),
                mode: spec.mode.clone(),
                new: view::short_sha(&spec.base),
            });
        }
        return Ok(vec![Step::Reuse { path }]);
    }
    if target_exists {
        return Err(AppError::NotAWorktree { path });
    }

    if let Some(branch) = &spec.branch
        && let Some(w) = facts
            .worktrees
            .iter()
            .find(|w| w.branch.as_ref() == Some(branch))
    {
        return Err(AppError::BranchCheckedOut {
            branch: branch.clone(),
            path: config::native(&w.path),
            hint: if w.prunable { prunable_hint(w) } else { "" },
        });
    }

    let arm = add_arm(facts, spec)?;
    let path_str = path.display().to_string();
    let meta = Meta {
        mode: Some(spec.mode.clone()),
        path: Some(path_str.clone()),
        branch: spec.branch.clone(),
        base: Some(spec.base.clone()),
        detached: Some(spec.detach),
        created: Some(parse::iso_utc(facts.now)),
        pr_branch: spec.pr_branch.clone(),
        title: spec.title.clone(),
        id: spec.item_id.clone().filter(|id| !id.is_empty()),
        via: spec.via.clone(),
        ..Meta::default()
    };
    let mut steps = vec![
        Step::AddWorktree {
            path: path.clone(),
            arm,
        },
        Step::Seed {
            from: facts.main_root.clone(),
            to: path,
            copy: conf.copy.clone(),
            symlink: conf.symlink.clone(),
        },
        Step::WriteSidecar {
            file: facts.meta_dir.join(parse::sidecar_file_name(leaf)),
            meta,
        },
    ];

    if !conf.exec.is_empty() {
        steps.push(exec_step(spec, conf, &path_str)?);
    }
    Ok(steps)
}

/// F17: a sidecar written before bases were resolved (`origin/x`) says
/// nothing about where the tree is pinned, so it is not compared.
#[must_use]
pub fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The `BranchCheckedOut` hint for a prunable match: one text when the
/// directory is actually gone (`wt rm` alone cleans it up), another when
/// only its `.git` link is broken (the directory needs manual attention
/// first - `wt rm` refuses it outright, see `plan_remove`).
const fn prunable_hint(w: &Worktree) -> &'static str {
    if w.gone {
        " (its directory is gone; `wt rm <name>` cleans it up)"
    } else {
        " (its .git link is broken; delete the directory or run `git worktree repair`)"
    }
}

/// Where `spec`'s tree goes, in native form, so the shell can check the
/// filesystem there before planning. Refuses a path outside the mode's root.
pub fn target_path(facts: &Facts, spec: &CreateSpec, conf: &Options) -> Result<PathBuf, AppError> {
    leaf_and_path(facts, spec, conf).map(|(_, path)| path)
}

fn leaf_and_path<'a>(
    facts: &Facts,
    spec: &'a CreateSpec,
    conf: &Options,
) -> Result<(&'a str, PathBuf), AppError> {
    let leaf = spec
        .dirname
        .as_deref()
        .or(spec.branch.as_deref())
        .filter(|l| !l.is_empty())
        .ok_or_else(|| AppError::NoLeaf {
            mode: spec.mode.clone(),
        })?;
    let wt_root = config::wt_root_of(&facts.main_root, conf);
    let path = config::normalize_lexical(&wt_root.join(leaf));
    if config::strip_under(&path, &wt_root).is_none_or(|rest| rest.as_os_str().is_empty()) {
        return Err(AppError::OutsideRoot {
            root: config::native(&wt_root),
            path: config::native(&path),
        });
    }
    Ok((leaf, config::native(&path)))
}

/// Python's `add_worktree` arms: detached, an existing branch checked out,
/// or a new branch (guessed from the remote with `track`).
fn add_arm(facts: &Facts, spec: &CreateSpec) -> Result<AddArm, AppError> {
    Ok(match &spec.branch {
        _ if spec.detach => AddArm::Detach {
            base: spec.base.clone(),
        },
        Some(branch) if facts.branches.contains(branch) => AddArm::Existing {
            branch: branch.clone(),
        },
        Some(branch) if spec.track => AddArm::GuessRemote {
            branch: branch.clone(),
        },
        Some(branch) => AddArm::New {
            branch: branch.clone(),
            base: spec.base.clone(),
        },
        None => {
            return Err(AppError::NothingToCheckOut {
                mode: spec.mode.clone(),
            });
        }
    })
}

/// The exec templates, filled in. Shared with `--open`, which hands them to
/// the new workspace's shell rather than running them here.
pub fn exec_cmds(spec: &CreateSpec, conf: &Options, path: &str) -> Result<Vec<String>, AppError> {
    let vars = [
        ("branch", spec.branch.as_deref().unwrap_or_default()),
        ("id", spec.item_id.as_deref().unwrap_or_default()),
        ("path", path),
    ];
    conf.exec.iter().map(|t| naming::fill(t, &vars)).collect()
}

fn exec_step(spec: &CreateSpec, conf: &Options, path: &str) -> Result<Step, AppError> {
    let cmds = exec_cmds(spec, conf, path)?;
    Ok(if spec.defer_exec {
        Step::DeferExec {
            strict: conf.exec_strict,
        }
    } else {
        Step::Exec {
            cmds,
            shell: conf.shell.clone(),
            timeout: Duration::from_secs(u64::try_from(conf.exec_timeout).unwrap_or(0)),
            strict: conf.exec_strict,
        }
    })
}

/// One of `wt item`'s post-create steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Publish {
    Push,
    Link,
    State(String),
}

/// `wt item`'s post-create steps under `conf`: push (default on), link the
/// branch (default off), set the state (when one is configured). Link and
/// state stay two PATCH requests: in one document, a relation that already
/// exists would fail the whole patch and the state would never be set.
#[must_use]
pub fn publish_steps(conf: &Options) -> Vec<Publish> {
    let mut steps = Vec::new();
    if conf.push {
        steps.push(Publish::Push);
    }
    if conf.link_branch.unwrap_or(false) {
        steps.push(Publish::Link);
    }
    if let Some(state) = conf.set_state.as_deref().filter(|s| !s.is_empty()) {
        steps.push(Publish::State(state.to_owned()));
    }
    steps
}

/// Runs `steps` through `run`, in order. A failed push ends it: linking or
/// activating a work item for a branch that is not on origin tells the
/// tracker something untrue.
pub fn publish(
    steps: &[Publish],
    mut run: impl FnMut(&Publish) -> provider::Outcome,
) -> Vec<provider::Outcome> {
    let mut out = Vec::new();
    for step in steps {
        let outcome = run(step);
        let push_failed = *step == Publish::Push && outcome.is_failure();
        out.push(outcome);
        if push_failed {
            out.push(provider::Outcome::failed(
                "skipped link/state: branch is not on origin",
            ));
            break;
        }
    }
    out
}

/// The dirty probes' raw results for one tree, gathered by `run.rs`. Which
/// range to count, and which reasons follow, is decided here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyFacts {
    /// `status --porcelain`: whether it listed anything, or git's error.
    pub status: Result<bool, String>,
    /// `rev-parse --abbrev-ref HEAD`: `None` when it failed, `"HEAD"` when
    /// detached.
    pub head: Option<String>,
    /// `@{u}`, empty when there is none or it no longer resolves.
    pub upstream: String,
    /// The upstream git reports `[gone]` for this branch, if it does.
    pub gone: Option<String>,
    /// `{remote}/{default branch}`.
    pub base: String,
    /// `rev-list --count` from `count_base` to the branch tip: `None` when
    /// it failed.
    pub ahead: Option<String>,
}

impl DirtyFacts {
    /// A prunable tree's directory is gone: there is nothing left to probe,
    /// and nothing to lose by skipping the probes. Shaped like a detached
    /// HEAD (`dirty_reasons` already treats that as nothing to push), so it
    /// reads as clean without inventing a new reason code.
    #[must_use]
    pub fn nothing_to_probe() -> Self {
        Self {
            status: Ok(false),
            head: Some("HEAD".to_owned()),
            upstream: String::new(),
            gone: None,
            base: String::new(),
            ahead: None,
        }
    }
}

/// The ref the branch's commits are counted against as unpushed (the range
/// is `<this>..<branch tip>`), or `None` when there is nothing to count: a
/// detached HEAD, or a branch whose upstream is gone.
///
/// A gone upstream is the squash-merge shape: the remote branch was deleted
/// when the PR completed, and counting against the base would call the
/// landed (squashed) commits unpushed. Skipping the count loses nothing:
/// the branch ref outlives the tree, and `if_merged`'s `branch -d` still
/// refuses a branch that is nowhere else. A gone upstream that is the base
/// itself is a missing base ref, the "could not compare" case, so it counts.
#[must_use]
pub fn count_base(d: &DirtyFacts) -> Option<&str> {
    d.head.as_deref().filter(|h| *h != "HEAD")?;
    if !d.upstream.is_empty() {
        return Some(&d.upstream);
    }
    d.gone
        .as_deref()
        .is_none_or(|g| g == d.base)
        .then_some(d.base.as_str())
}

/// Why this tree is unsafe to delete; empty means verified clean. Every
/// probe fails closed: a failed one is a reason, never silence. No stash
/// probe: `refs/stash` is shared by every worktree in the repo, and removing
/// a tree does not touch it.
#[must_use]
pub fn dirty_reasons(d: &DirtyFacts) -> Vec<String> {
    let mut reasons = Vec::new();
    match &d.status {
        Err(e) => reasons.push(format!("could not read status: {e}")),
        Ok(true) => reasons.push("uncommitted changes".to_owned()),
        Ok(false) => {}
    }
    if d.head.is_none() {
        reasons.push("could not read HEAD".to_owned());
    }
    if let Some(lhs) = count_base(d) {
        match d.ahead.as_deref().map(str::trim) {
            None => reasons.push(format!("could not compare against {lhs}")),
            Some("" | "0") => {}
            Some(_) => reasons.push("unpushed commits".to_owned()),
        }
    }
    reasons
}

/// What teardown decided for one tree, before anything is done about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub path: PathBuf,
    /// `None`: no readable sidecar, removed under the default policy.
    pub sidecar: Option<PathBuf>,
    pub mode: Option<String>,
    pub teardown: Teardown,
    pub reasons: Vec<String>,
    pub branch: Option<String>,
    /// The copied entries that sit inside the tree.
    pub purge: Vec<String>,
}

/// One thing a removal does, in order. `run.rs` stops at the first failure,
/// so a later step (the sidecar above all) survives a removal that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveStep {
    Purge {
        tree: PathBuf,
        rels: Vec<String>,
    },
    RemoveCheckout {
        path: PathBuf,
        force: bool,
    },
    /// `branch -D` when `force`, else `branch -d`.
    DeleteBranch {
        branch: String,
        force: bool,
    },
    DropSidecar {
        file: PathBuf,
    },
    Prune,
}

/// Resolves the policy for `target`. Refuses the main worktree, a prunable
/// tree whose directory is still present (its `.git` link is merely broken:
/// git itself refuses to remove it, even with `--force`, and a dirty probe
/// run there could walk up into an enclosing repo and read as clean - see
/// `BrokenGitLink`), and a tree with no readable sidecar unless `force`: an
/// unknown mode must not fall back to the branch-deleting default. `dry_run`
/// previews one anyway (F21), except a broken `.git` link, which is refused
/// outright before any probe or purge regardless of `force`/`dry_run`.
pub fn plan_remove(
    facts: &Facts,
    target: &Worktree,
    dirty: &DirtyFacts,
    force: bool,
    dry_run: bool,
) -> Result<Removal, AppError> {
    if config::same_path(&target.path, &facts.main_root) {
        return Err(AppError::MainWorktree);
    }
    if target.prunable && !target.gone {
        return Err(AppError::BrokenGitLink {
            path: config::native(&target.path),
        });
    }
    let path = config::native(&target.path);
    let found = select::sidecar_for_path(&facts.sidecars, &path);
    if found.is_none() && !force && !dry_run {
        return Err(AppError::NoMetadata { path });
    }
    let meta = found.map(|(_, m)| m);
    let mode = meta.and_then(|m| m.mode.clone());
    let purge = meta
        .map(|m| {
            m.copied
                .iter()
                .filter(|rel| {
                    let entry = config::normalize_lexical(&path.join(rel));
                    config::strip_under(&entry, &path).is_some_and(|r| !r.as_os_str().is_empty())
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Ok(Removal {
        teardown: config::resolve_teardown(&facts.cfg, mode.as_deref().unwrap_or("new")),
        sidecar: found.map(|(file, _)| file.clone()),
        reasons: dirty_reasons(dirty),
        branch: meta
            .and_then(|m| m.branch.clone())
            .or_else(|| target.branch.clone()),
        mode,
        purge,
        path,
    })
}

/// The steps that carry out `r`, or the refusal of a dirty tree. The purge
/// comes first: `git worktree remove` takes the directory with it, and if
/// the removal fails the copied secrets must not sit in a tree nobody
/// tracks. The sidecar goes only after the tree is gone: while it is on
/// disk, the sidecar is the only record of which policy applies.
pub fn removal_steps(r: &Removal, force: bool) -> Result<Vec<RemoveStep>, AppError> {
    let td = &r.teardown;
    if td.require_clean && !force && !r.reasons.is_empty() {
        return Err(AppError::NotClean {
            path: r.path.clone(),
            reasons: r.reasons.join(", "),
        });
    }
    let mut steps = Vec::new();
    if td.purge_copied && !r.purge.is_empty() {
        steps.push(RemoveStep::Purge {
            tree: r.path.clone(),
            rels: r.purge.clone(),
        });
    }
    steps.push(RemoveStep::RemoveCheckout {
        path: r.path.clone(),
        force,
    });
    if let Some(branch) = &r.branch
        && td.delete_branch != "never"
    {
        steps.push(RemoveStep::DeleteBranch {
            branch: branch.clone(),
            force: td.delete_branch == "always",
        });
    }
    if let Some(file) = &r.sidecar {
        steps.push(RemoveStep::DropSidecar { file: file.clone() });
    }
    if td.prune {
        steps.push(RemoveStep::Prune);
    }
    Ok(steps)
}

// ------------------------------------------------------------------- sweeps

/// One worktree a sweep (`--stale`, `--merged`) will remove, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    pub tree: Worktree,
    pub why: String,
}

/// One worktree a sweep leaves alone, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub tree: Worktree,
    pub why: String,
}

/// Splits `trees` before either sweep judges them: a prunable tree whose
/// directory is still present cannot be probed or removed at all (its
/// `.git` link is broken; see `plan_remove`/`BrokenGitLink`), so a sweep
/// skips it up front rather than discovering that only when the removal
/// itself refuses partway through.
#[must_use]
pub fn skip_broken_prunable(trees: Vec<Worktree>) -> (Vec<Worktree>, Vec<Skipped>) {
    let mut checkable = Vec::new();
    let mut skipped = Vec::new();
    for tree in trees {
        if tree.prunable && !tree.gone {
            skipped.push(Skipped {
                tree,
                why: "its .git link is broken".to_owned(),
            });
        } else {
            checkable.push(tree);
        }
    }
    (checkable, skipped)
}

/// Splits `trees` by each one's own mode's `ttl_days`: due once `created` is
/// older than the TTL, skipped when there is nothing to judge it by. F9: a
/// `created` that is missing or does not parse is never stale, rather than
/// crashing or reaping on a guess.
#[must_use]
pub fn stale_targets(
    trees: &[Worktree],
    sidecars: &[(PathBuf, Meta)],
    cfg: &toml::Table,
    now: jiff::Timestamp,
) -> (Vec<Due>, Vec<Skipped>) {
    let mut due = Vec::new();
    let mut skipped = Vec::new();
    for w in trees {
        let meta = select::meta_for_path(sidecars, &w.path);
        let Some(created) = meta.and_then(|m| m.created.as_deref()) else {
            skipped.push(Skipped {
                tree: w.clone(),
                why: "no readable wt metadata; cannot tell how old it is".to_owned(),
            });
            continue;
        };
        let Ok(created) = created.parse::<jiff::Timestamp>() else {
            skipped.push(Skipped {
                tree: w.clone(),
                why: "created is not a valid timestamp; cannot tell how old it is".to_owned(),
            });
            continue;
        };
        let mode = meta
            .and_then(|m| m.mode.clone())
            .unwrap_or_else(|| "new".to_owned());
        // Python's `if not ttl:` treats 0 (and, defensively, a negative
        // value) the same as unset, not "always due".
        let ttl = config::resolve_teardown(cfg, &mode)
            .ttl_days
            .filter(|ttl| *ttl > 0);
        let Some(ttl) = ttl else {
            skipped.push(Skipped {
                tree: w.clone(),
                why: format!("no ttl_days configured for mode '{mode}'"),
            });
            continue;
        };
        let Some(age_secs) = now.as_second().checked_sub(created.as_second()) else {
            continue;
        };
        let Some(threshold) = ttl.checked_mul(86_400) else {
            continue;
        };
        if age_secs > threshold {
            let days = age_secs.checked_div(86_400).unwrap_or(0);
            due.push(Due {
                tree: w.clone(),
                why: format!("stale ({days}d)"),
            });
        }
    }
    (due, skipped)
}

/// Splits `trees` by whether their branch has landed on `base`
/// (`{remote}/{default branch}`): `merged` is `git branch --merged base`'s
/// result, `gone` is `gone_upstreams` (a squash lands as a new commit, so
/// ancestry cannot see it; its remote branch disappearing is what is left).
/// A detached tree has no branch to have merged, so it is skipped rather
/// than reaped; `--stale` is what clears review trees.
///
/// F1 gates only the ancestry match on `history` (each branch's
/// `BranchHistory`): a branch whose reflog holds no commit of its own is
/// spared even when it matches `merged` - it only looks merged because it
/// is identical to the default branch. The gone-upstream match is not
/// gated: a `--guess-remote` tree's own reflog only ever shows its
/// creation (the real work already existed on the remote branch it
/// tracked, never committed locally), so `history` there is not evidence
/// of anything; the dirty check and `delete_branch = "if_merged"`'s `-d`
/// already keep genuinely unmerged work from being lost.
#[must_use]
pub fn merged_targets(
    trees: &[Worktree],
    merged: &BTreeSet<String>,
    gone: &BTreeMap<String, String>,
    history: &BTreeMap<String, BranchHistory>,
    base: &str,
) -> (Vec<Due>, Vec<Skipped>) {
    let mut due = Vec::new();
    let mut skipped = Vec::new();
    for w in trees {
        let Some(branch) = &w.branch else {
            skipped.push(Skipped {
                tree: w.clone(),
                why: "detached, so it has no branch to merge".to_owned(),
            });
            continue;
        };
        if merged.contains(branch) {
            if history.get(branch) == Some(&BranchHistory::HasCommits) {
                due.push(Due {
                    tree: w.clone(),
                    why: format!("merged into {base}"),
                });
            } else {
                skipped.push(Skipped {
                    tree: w.clone(),
                    why: "no commits ever landed on it; sparing a fresh branch, not a merged one"
                        .to_owned(),
                });
            }
            continue;
        }
        if let Some(up) = gone.get(branch).filter(|up| up.as_str() != base) {
            due.push(Due {
                tree: w.clone(),
                why: format!("upstream {up} is gone"),
            });
        }
    }
    (due, skipped)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn facts() -> Facts {
        Facts {
            main_root: PathBuf::from("/r/repo"),
            meta_dir: PathBuf::from("/r/repo/.git/wt"),
            config_dir: PathBuf::from("/r/wt-home"),
            cfg: toml::Table::new(),
            provider: "none",
            remote: "origin".to_owned(),
            worktrees: vec![Worktree {
                path: PathBuf::from("/r/repo"),
                branch: Some("main".to_owned()),
                head: "abc".to_owned(),
                prunable: false,
                gone: false,
            }],
            sidecars: vec![],
            branches: vec!["main".to_owned()],
            now: "2026-09-28T10:11:12.123456Z".parse().unwrap(),
        }
    }

    fn conf(extra: &str) -> Options {
        let cfg = config::deep_merge(&config::defaults(), &toml::from_str(extra).unwrap());
        config::resolve(&cfg, "new", "none")
    }

    fn spec(branch: &str) -> CreateSpec {
        CreateSpec {
            mode: "new".to_owned(),
            branch: Some(branch.to_owned()),
            base: "origin/main".to_owned(),
            ..CreateSpec::default()
        }
    }

    fn tree(leaf: &str) -> PathBuf {
        config::native(Path::new(&format!("/r/repo.worktrees/{leaf}")))
    }

    #[test]
    fn a_new_branch_is_added_seeded_and_recorded() {
        let steps = plan_create(
            &facts(),
            &spec("feat/x"),
            &conf("[defaults]\ncopy = [\".env\"]\n"),
            false,
        )
        .unwrap();
        let path = tree("feat/x");
        assert_eq!(
            steps,
            vec![
                Step::AddWorktree {
                    path: path.clone(),
                    arm: AddArm::New {
                        branch: "feat/x".to_owned(),
                        base: "origin/main".to_owned()
                    },
                },
                Step::Seed {
                    from: PathBuf::from("/r/repo"),
                    to: path.clone(),
                    copy: vec![".env".to_owned()],
                    symlink: vec![],
                },
                Step::WriteSidecar {
                    file: PathBuf::from("/r/repo/.git/wt/feat%2Fx.json"),
                    meta: Meta {
                        mode: Some("new".to_owned()),
                        path: Some(path.display().to_string()),
                        branch: Some("feat/x".to_owned()),
                        base: Some("origin/main".to_owned()),
                        detached: Some(false),
                        created: Some("2026-09-28T10:11:12.123456+00:00".to_owned()),
                        ..Meta::default()
                    },
                },
            ]
        );
    }

    fn arm(steps: &[Step]) -> &AddArm {
        steps
            .iter()
            .find_map(|s| match s {
                Step::AddWorktree { arm, .. } => Some(arm),
                _ => None,
            })
            .unwrap()
    }

    #[test]
    fn an_existing_branch_is_checked_out_not_created() {
        let mut facts = facts();
        facts.branches.push("feat/x".to_owned());
        let steps = plan_create(&facts, &spec("feat/x"), &conf(""), false).unwrap();
        assert_eq!(
            arm(&steps),
            &AddArm::Existing {
                branch: "feat/x".to_owned()
            }
        );
        assert_eq!(arm(&steps).created_branch(), None);
    }

    #[test]
    fn track_guesses_the_remote_and_detach_pins_the_base() {
        let tracked = CreateSpec {
            track: true,
            ..spec("feat/x")
        };
        let steps = plan_create(&facts(), &tracked, &conf(""), false).unwrap();
        assert_eq!(
            arm(&steps),
            &AddArm::GuessRemote {
                branch: "feat/x".to_owned()
            }
        );
        assert_eq!(arm(&steps).created_branch(), Some("feat/x"));

        let detached = CreateSpec {
            branch: None,
            detach: true,
            dirname: Some("review/pr-1".to_owned()),
            base: "abc123".to_owned(),
            item_id: Some("1".to_owned()),
            pr_branch: Some("feat/x".to_owned()),
            ..spec("")
        };
        let steps = plan_create(&facts(), &detached, &conf(""), false).unwrap();
        assert_eq!(
            arm(&steps),
            &AddArm::Detach {
                base: "abc123".to_owned()
            }
        );
        let Some(Step::WriteSidecar { file, meta }) = steps.get(2) else {
            panic!("{steps:?}");
        };
        assert_eq!(file, Path::new("/r/repo/.git/wt/review%2Fpr-1.json"));
        assert_eq!(meta.branch, None);
        assert_eq!(meta.detached, Some(true));
        assert_eq!(meta.pr_branch.as_deref(), Some("feat/x"));
        assert_eq!(meta.id.as_deref(), Some("1"));
    }

    #[test]
    fn test_refuses_to_create_outside_the_root() {
        let escape = CreateSpec {
            branch: None,
            detach: true,
            dirname: Some("review/../../../escaped".to_owned()),
            ..spec("")
        };
        let err = plan_create(&facts(), &escape, &conf(""), false).unwrap_err();
        assert!(
            err.to_string().starts_with(&format!(
                "refusing to create outside {}: ",
                config::native(Path::new("/r/repo.worktrees")).display()
            )),
            "{err}"
        );
    }

    #[test]
    fn a_registered_tree_is_reused() {
        let mut facts = facts();
        facts.worktrees.push(Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/x"),
            branch: Some("feat/x".to_owned()),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        });
        // A registered tree is on disk too; that is not a refusal.
        let steps = plan_create(&facts, &spec("feat/x"), &conf(""), true).unwrap();
        assert_eq!(
            steps,
            vec![Step::Reuse {
                path: tree("feat/x")
            }]
        );
    }

    #[test]
    fn an_unregistered_path_on_disk_is_refused() {
        let err = plan_create(&facts(), &spec("feat/x"), &conf(""), true).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{} exists but is not a worktree; remove it or pick another name",
                tree("feat/x").display()
            )
        );
    }

    #[test]
    fn target_path_is_where_plan_create_puts_the_tree() {
        assert_eq!(
            target_path(&facts(), &spec("feat/x"), &conf("")).unwrap(),
            tree("feat/x")
        );
    }

    const OLD_SHA: &str = "1111111122222222333333334444444455555555";
    const NEW_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Facts with a detached `review/pr-1` tree whose sidecar recorded `base`.
    fn pinned_facts(base: &str) -> Facts {
        let mut facts = facts();
        facts.worktrees.push(Worktree {
            path: PathBuf::from("/r/repo.worktrees/review/pr-1"),
            branch: None,
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        });
        facts.sidecars.push((
            PathBuf::from("/r/repo/.git/wt/review%2Fpr-1.json"),
            Meta {
                path: Some(tree("review/pr-1").display().to_string()),
                base: Some(base.to_owned()),
                ..Meta::default()
            },
        ));
        facts
    }

    #[test]
    fn f17_an_old_sidecar_whose_base_is_not_a_sha_skips_the_pin_check() {
        let facts = pinned_facts("origin/feat/x");
        let spec = CreateSpec {
            mode: "branch".to_owned(),
            branch: None,
            detach: true,
            dirname: Some("review/pr-1".to_owned()),
            base: NEW_SHA.to_owned(),
            ..spec("")
        };
        assert!(matches!(
            plan_create(&facts, &spec, &conf(""), false)
                .unwrap()
                .as_slice(),
            [Step::Reuse { .. }]
        ));
    }

    #[test]
    fn test_refuses_when_the_pinned_base_moved() {
        let facts = pinned_facts(OLD_SHA);
        let moved = CreateSpec {
            mode: "pr".to_owned(),
            branch: None,
            detach: true,
            dirname: Some("review/pr-1".to_owned()),
            base: NEW_SHA.to_owned(),
            ..spec("")
        };
        let err = plan_create(&facts, &moved, &conf(""), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{} is pinned to 11111111 but pr now resolves to 01234567; remove it and re-create",
                tree("review/pr-1").display()
            )
        );
        let same = CreateSpec {
            base: OLD_SHA.to_owned(),
            ..moved
        };
        assert!(matches!(
            plan_create(&facts, &same, &conf(""), false)
                .unwrap()
                .as_slice(),
            [Step::Reuse { .. }]
        ));
    }

    #[test]
    fn a_branch_checked_out_elsewhere_is_refused() {
        let err = plan_create(&facts(), &spec("main"), &conf(""), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "branch main is already checked out at {}",
                config::native(Path::new("/r/repo")).display()
            )
        );
    }

    #[test]
    fn a_branch_checked_out_in_a_prunable_tree_hints_at_rm() {
        let mut facts = facts();
        facts.worktrees.push(Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/gone"),
            branch: Some("feat/gone".to_owned()),
            head: "abc".to_owned(),
            prunable: true,
            gone: true,
        });
        let err = plan_create(&facts, &spec("feat/gone"), &conf(""), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "branch feat/gone is already checked out at {} (its directory is gone; `wt rm <name>` cleans it up)",
                config::native(Path::new("/r/repo.worktrees/feat/gone")).display()
            )
        );
    }

    #[test]
    fn a_detached_prunable_tree_at_the_target_path_also_refuses_with_hint() {
        // The pr/review shape: no branch, so `Reuse` (a directory that may
        // not even be there any more) must not be the fallback either.
        let mut facts = facts();
        facts.worktrees.push(Worktree {
            path: PathBuf::from("/r/repo.worktrees/review/pr-1"),
            branch: None,
            head: "abc".to_owned(),
            prunable: true,
            gone: true,
        });
        let spec = CreateSpec {
            mode: "pr".to_owned(),
            branch: None,
            detach: true,
            dirname: Some("review/pr-1".to_owned()),
            base: "abc123".to_owned(),
            ..spec("")
        };
        let err = plan_create(&facts, &spec, &conf(""), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "branch (detached) is already checked out at {} (its directory is gone; `wt rm <name>` cleans it up)",
                tree("review/pr-1").display()
            )
        );
    }

    #[test]
    fn a_branch_checked_out_in_a_present_prunable_tree_hints_at_repair_not_rm() {
        // Only the `.git` link is broken; the directory itself is still
        // there, so the hint must not claim it is gone.
        let mut facts = facts();
        facts.worktrees.push(Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/broken"),
            branch: Some("feat/broken".to_owned()),
            head: "abc".to_owned(),
            prunable: true,
            gone: false,
        });
        let err = plan_create(&facts, &spec("feat/broken"), &conf(""), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "branch feat/broken is already checked out at {} (its .git link is broken; delete the directory or run `git worktree repair`)",
                config::native(Path::new("/r/repo.worktrees/feat/broken")).display()
            )
        );
    }

    #[test]
    fn nothing_to_name_or_check_out_is_refused() {
        let nameless = CreateSpec {
            branch: None,
            ..spec("")
        };
        assert_eq!(
            plan_create(&facts(), &nameless, &conf(""), false)
                .unwrap_err()
                .to_string(),
            "new: no branch or directory name to create a worktree from"
        );
        let nothing = CreateSpec {
            branch: None,
            dirname: Some("x".to_owned()),
            ..spec("")
        };
        assert_eq!(
            plan_create(&facts(), &nothing, &conf(""), false)
                .unwrap_err()
                .to_string(),
            "new: nothing to check out - no branch and not detached"
        );
    }

    #[test]
    fn exec_steps_are_filled_and_bounded() {
        let conf = conf(
            "[defaults]\nexec = [\"echo {branch} {id} {path}\", \"if (1) { exit 0 }\"]\nexec_timeout = 5\nexec_strict = true\nshell = [\"sh\", \"-c\"]\n",
        );
        let steps = plan_create(&facts(), &spec("feat/x"), &conf, false).unwrap();
        assert_eq!(
            steps.last(),
            Some(&Step::Exec {
                cmds: vec![
                    format!("echo feat/x  {}", tree("feat/x").display()),
                    "if (1) { exit 0 }".to_owned()
                ],
                shell: Some(vec!["sh".to_owned(), "-c".to_owned()]),
                timeout: Duration::from_secs(5),
                strict: true,
            })
        );
        let deferred = CreateSpec {
            defer_exec: true,
            ..spec("feat/x")
        };
        assert!(matches!(
            plan_create(&facts(), &deferred, &conf, false)
                .unwrap()
                .last(),
            Some(Step::DeferExec { strict: true })
        ));
    }

    #[test]
    fn a_bad_exec_placeholder_fails_before_anything_is_made() {
        let conf = conf("[defaults]\nexec = [\"echo {slug}\"]\n");
        let err = plan_create(&facts(), &spec("feat/x"), &conf, false).unwrap_err();
        assert!(
            matches!(err, AppError::UnresolvedPlaceholder { .. }),
            "{err}"
        );
    }

    // ------------------------------------------------------------- dirty_reasons

    fn clean(head: &str) -> DirtyFacts {
        DirtyFacts {
            status: Ok(false),
            head: Some(head.to_owned()),
            upstream: String::new(),
            gone: None,
            base: "origin/main".to_owned(),
            ahead: Some("0".to_owned()),
        }
    }

    #[test]
    fn test_clean_tree_has_no_reasons() {
        assert_eq!(dirty_reasons(&clean("feat/x")), Vec::<String>::new());
        assert_eq!(count_base(&clean("feat/x")), Some("origin/main"));
    }

    #[test]
    fn test_uncommitted_changes_and_unpushed_commits() {
        let d = DirtyFacts {
            status: Ok(true),
            ahead: Some("2".to_owned()),
            ..clean("feat/x")
        };
        assert_eq!(
            dirty_reasons(&d),
            ["uncommitted changes", "unpushed commits"]
        );
    }

    #[test]
    fn an_upstream_is_counted_against_instead_of_the_base() {
        let d = DirtyFacts {
            upstream: "origin/feat/x".to_owned(),
            ..clean("feat/x")
        };
        assert_eq!(count_base(&d), Some("origin/feat/x"));
    }

    #[test]
    fn test_missing_tracking_ref_is_a_reason_not_silence() {
        let d = DirtyFacts {
            ahead: None,
            ..clean("feat/x")
        };
        assert_eq!(dirty_reasons(&d), ["could not compare against origin/main"]);
    }

    #[test]
    fn a_prunable_tree_has_nothing_to_probe_and_no_reasons() {
        assert_eq!(
            dirty_reasons(&DirtyFacts::nothing_to_probe()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn every_failed_probe_is_a_reason() {
        let d = DirtyFacts {
            status: Err("fatal: not a git repository".to_owned()),
            head: None,
            ..clean("")
        };
        assert_eq!(
            dirty_reasons(&d),
            [
                "could not read status: fatal: not a git repository",
                "could not read HEAD"
            ]
        );
    }

    #[test]
    fn a_detached_head_has_nothing_to_push() {
        let d = DirtyFacts {
            ahead: None,
            ..clean("HEAD")
        };
        assert_eq!(count_base(&d), None);
        assert_eq!(dirty_reasons(&d), Vec::<String>::new());
    }

    #[test]
    fn a_squash_merged_branch_whose_upstream_is_gone_is_not_counted() {
        let d = DirtyFacts {
            gone: Some("origin/feat/x".to_owned()),
            ahead: None,
            ..clean("feat/x")
        };
        assert_eq!(count_base(&d), None);
        assert_eq!(dirty_reasons(&d), Vec::<String>::new());
        // Gone to the base itself is a missing base ref: still counted.
        let d = DirtyFacts {
            gone: Some("origin/main".to_owned()),
            ahead: None,
            ..clean("feat/x")
        };
        assert_eq!(dirty_reasons(&d), ["could not compare against origin/main"]);
    }

    // --------------------------------------------------------------- plan_remove

    fn rm_facts(sidecar: Option<Meta>, extra: &str) -> (Facts, Worktree) {
        let mut facts = facts();
        facts.cfg = config::deep_merge(&config::defaults(), &toml::from_str(extra).unwrap());
        let target = Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/x"),
            branch: Some("feat/x".to_owned()),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        };
        facts.worktrees.push(target.clone());
        if let Some(meta) = sidecar {
            facts
                .sidecars
                .push((PathBuf::from("/r/repo/.git/wt/feat%2Fx.json"), meta));
        }
        (facts, target)
    }

    fn sidecar(mode: &str, copied: &[&str]) -> Meta {
        Meta {
            mode: Some(mode.to_owned()),
            // As the Python tool writes it on Windows: native separators.
            path: Some(tree("feat/x").display().to_string()),
            branch: Some("feat/x".to_owned()),
            copied: copied.iter().map(|c| (*c).to_owned()).collect(),
            ..Meta::default()
        }
    }

    #[test]
    fn test_refuses_to_remove_main() {
        let (facts, _) = rm_facts(None, "");
        let main = facts.worktrees.first().unwrap().clone();
        let err = plan_remove(&facts, &main, &clean("main"), true, false).unwrap_err();
        assert_eq!(err.to_string(), "refusing to remove the main worktree");
    }

    #[test]
    fn a_prunable_tree_whose_directory_is_still_present_is_refused_outright() {
        // Its `.git` link is broken, not its directory: git itself refuses
        // to remove it even with --force, and a dirty probe run there could
        // walk upward into an enclosing repo and read as clean. Refused
        // before any probe or purge, regardless of force/dry_run.
        let (facts, mut target) = rm_facts(Some(sidecar("new", &[".env"])), "");
        target.prunable = true;
        target.gone = false;
        for (force, dry_run) in [(false, false), (true, false), (false, true), (true, true)] {
            let err = plan_remove(&facts, &target, &clean("feat/x"), force, dry_run).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!(
                    "{}: its .git link is broken, so git cannot remove it; delete the directory (or run `git worktree repair`) and run `wt rm` again",
                    tree("feat/x").display()
                ),
                "force={force} dry_run={dry_run}"
            );
        }
    }

    #[test]
    fn a_prunable_tree_that_is_actually_gone_is_not_refused_outright() {
        let (facts, mut target) = rm_facts(Some(sidecar("new", &[])), "");
        target.prunable = true;
        target.gone = true;
        let r = plan_remove(
            &facts,
            &target,
            &DirtyFacts::nothing_to_probe(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(r.mode.as_deref(), Some("new"));
    }

    #[test]
    fn test_refuses_without_sidecar() {
        let (facts, target) = rm_facts(None, "");
        let err = plan_remove(&facts, &target, &clean("feat/x"), false, false).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "{}: no readable wt metadata. Re-run with --force to remove it under the default policy.",
                tree("feat/x").display()
            )
        );
    }

    #[test]
    fn f21_dry_run_previews_a_tree_without_a_sidecar() {
        let (facts, target) = rm_facts(None, "");
        for (force, dry_run) in [(false, true), (true, false)] {
            let r = plan_remove(&facts, &target, &clean("feat/x"), force, dry_run).unwrap();
            assert_eq!(r.sidecar, None);
            assert_eq!(r.mode, None);
            assert_eq!(r.teardown, config::resolve_teardown(&facts.cfg, "new"));
            assert_eq!(r.branch.as_deref(), Some("feat/x"), "the tree's own branch");
        }
    }

    #[test]
    fn the_sidecar_is_found_by_path_and_its_mode_decides_the_policy() {
        let (facts, target) = rm_facts(
            Some(sidecar("pr", &[])),
            "[teardown.mode.pr]\nrequire_clean = false\ndelete_branch = \"never\"\n",
        );
        let r = plan_remove(&facts, &target, &clean("feat/x"), false, false).unwrap();
        assert_eq!(
            r.sidecar,
            Some(PathBuf::from("/r/repo/.git/wt/feat%2Fx.json"))
        );
        assert_eq!(r.mode.as_deref(), Some("pr"));
        assert_eq!(r.teardown.delete_branch, "never");
        assert!(!r.teardown.require_clean);
        assert_eq!(r.path, tree("feat/x"));
    }

    #[test]
    fn test_purge_ignores_paths_outside_the_worktree() {
        let (facts, target) = rm_facts(
            Some(sidecar(
                "new",
                &[".env", "../../../canary.txt", "sub/../../x", ".", "/abs"],
            )),
            "",
        );
        let r = plan_remove(&facts, &target, &clean("feat/x"), false, false).unwrap();
        assert_eq!(r.purge, [".env"]);
    }

    #[test]
    fn the_dirty_reasons_are_carried() {
        let (facts, target) = rm_facts(Some(sidecar("new", &[])), "");
        let dirty = DirtyFacts {
            status: Ok(true),
            ..clean("feat/x")
        };
        let r = plan_remove(&facts, &target, &dirty, false, false).unwrap();
        assert_eq!(r.reasons, ["uncommitted changes"]);
    }

    // ------------------------------------------------------------- removal_steps

    fn removal(extra: &str) -> Removal {
        let (facts, target) = rm_facts(Some(sidecar("new", &[".env"])), extra);
        plan_remove(&facts, &target, &clean("feat/x"), false, false).unwrap()
    }

    #[test]
    fn a_default_removal_purges_removes_drops_the_merged_branch_and_prunes() {
        assert_eq!(
            removal_steps(&removal(""), false).unwrap(),
            [
                RemoveStep::Purge {
                    tree: tree("feat/x"),
                    rels: vec![".env".to_owned()]
                },
                RemoveStep::RemoveCheckout {
                    path: tree("feat/x"),
                    force: false
                },
                RemoveStep::DeleteBranch {
                    branch: "feat/x".to_owned(),
                    force: false
                },
                RemoveStep::DropSidecar {
                    file: PathBuf::from("/r/repo/.git/wt/feat%2Fx.json")
                },
                RemoveStep::Prune,
            ]
        );
    }

    fn branch_step(extra: &str) -> Option<RemoveStep> {
        removal_steps(&removal(extra), true)
            .unwrap()
            .into_iter()
            .find(|s| matches!(s, RemoveStep::DeleteBranch { .. }))
    }

    #[test]
    fn delete_branch_never_if_merged_always() {
        assert_eq!(branch_step("[teardown]\ndelete_branch = \"never\"\n"), None);
        assert_eq!(
            branch_step("[teardown]\ndelete_branch = \"if_merged\"\n"),
            Some(RemoveStep::DeleteBranch {
                branch: "feat/x".to_owned(),
                force: false
            })
        );
        assert_eq!(
            branch_step("[teardown]\ndelete_branch = \"always\"\n"),
            Some(RemoveStep::DeleteBranch {
                branch: "feat/x".to_owned(),
                force: true
            })
        );
    }

    #[test]
    fn a_detached_tree_has_no_branch_to_delete() {
        let r = Removal {
            branch: None,
            ..removal("")
        };
        assert!(
            !removal_steps(&r, false)
                .unwrap()
                .iter()
                .any(|s| matches!(s, RemoveStep::DeleteBranch { .. }))
        );
    }

    #[test]
    fn purge_and_prune_follow_the_policy() {
        let steps = removal_steps(
            &removal("[teardown]\npurge_copied = false\nprune = false\n"),
            true,
        )
        .unwrap();
        assert!(
            matches!(
                steps.as_slice(),
                [
                    RemoveStep::RemoveCheckout { force: true, .. },
                    RemoveStep::DeleteBranch { .. },
                    RemoveStep::DropSidecar { .. }
                ]
            ),
            "{steps:?}"
        );
    }

    #[test]
    fn test_refuses_dirty_tree_unless_forced_or_policy_allows() {
        let dirty = Removal {
            reasons: vec![
                "uncommitted changes".to_owned(),
                "unpushed commits".to_owned(),
            ],
            ..removal("")
        };
        assert_eq!(
            removal_steps(&dirty, false).unwrap_err().to_string(),
            format!(
                "{}: uncommitted changes, unpushed commits (use --force to override)",
                tree("feat/x").display()
            )
        );
        assert!(removal_steps(&dirty, true).is_ok());
        let lax = Removal {
            reasons: dirty.reasons,
            ..removal("[teardown]\nrequire_clean = false\n")
        };
        assert!(removal_steps(&lax, false).is_ok());
    }

    #[test]
    fn a_missing_sidecar_has_nothing_to_drop() {
        let r = Removal {
            sidecar: None,
            ..removal("")
        };
        assert!(
            !removal_steps(&r, true)
                .unwrap()
                .iter()
                .any(|s| matches!(s, RemoveStep::DropSidecar { .. }))
        );
    }

    // ---------------------------------------------------- skip_broken_prunable

    #[test]
    fn skip_broken_prunable_skips_only_a_present_prunable_tree() {
        let healthy = Worktree {
            prunable: false,
            gone: false,
            ..stale_tree_fixture()
        };
        let gone = Worktree {
            prunable: true,
            gone: true,
            ..stale_tree_fixture()
        };
        let broken = Worktree {
            prunable: true,
            gone: false,
            ..stale_tree_fixture()
        };
        let (checkable, skipped) =
            skip_broken_prunable(vec![healthy.clone(), gone.clone(), broken.clone()]);
        assert_eq!(checkable, vec![healthy, gone]);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].tree, broken);
        assert_eq!(skipped[0].why, "its .git link is broken");
    }

    fn stale_tree_fixture() -> Worktree {
        Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/x"),
            branch: Some("feat/x".to_owned()),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        }
    }

    // -------------------------------------------------------- stale_targets

    fn stale_tree(leaf: &str) -> Worktree {
        Worktree {
            path: tree(leaf),
            branch: Some(leaf.to_owned()),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        }
    }

    fn stale_sidecar(leaf: &str, mode: &str, created: &str) -> (PathBuf, Meta) {
        (
            PathBuf::from(format!("/r/repo/.git/wt/{}.json", leaf.replace('/', "%2F"))),
            Meta {
                mode: Some(mode.to_owned()),
                path: Some(tree(leaf).display().to_string()),
                created: Some(created.to_owned()),
                ..Meta::default()
            },
        )
    }

    fn stale_cfg(ttl_days: i64) -> toml::Table {
        config::deep_merge(
            &config::defaults(),
            &toml::from_str(&format!("[teardown.mode.pr]\nttl_days = {ttl_days}\n")).unwrap(),
        )
    }

    const NOW: &str = "2026-09-28T00:00:00Z";

    #[test]
    fn test_reaps_past_ttl() {
        let trees = vec![stale_tree("feat/old")];
        let sidecars = vec![stale_sidecar("feat/old", "pr", "2026-09-19T00:00:00Z")];
        let (due, skipped) = stale_targets(&trees, &sidecars, &stale_cfg(3), NOW.parse().unwrap());
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].why, "stale (9d)");
    }

    #[test]
    fn test_spares_inside_ttl() {
        let trees = vec![stale_tree("feat/young")];
        let sidecars = vec![stale_sidecar("feat/young", "pr", "2026-09-27T00:00:00Z")];
        let (due, skipped) = stale_targets(&trees, &sidecars, &stale_cfg(3), NOW.parse().unwrap());
        assert!(due.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn test_spares_modes_without_ttl() {
        let trees = vec![stale_tree("feat/forever")];
        let sidecars = vec![stale_sidecar("feat/forever", "new", "2025-01-01T00:00:00Z")];
        let (due, skipped) = stale_targets(&trees, &sidecars, &stale_cfg(3), NOW.parse().unwrap());
        assert!(due.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].why, "no ttl_days configured for mode 'new'");
    }

    #[test]
    fn a_ttl_days_of_zero_or_negative_is_treated_as_unset() {
        // Python's `if not ttl:` skips 0 the same as missing; a negative
        // value (never valid config, but defend it the same way) must not
        // make every tree due immediately either.
        for ttl in [0, -1] {
            let trees = vec![stale_tree("feat/zero")];
            let sidecars = vec![stale_sidecar("feat/zero", "pr", "2020-01-01T00:00:00Z")];
            let (due, skipped) =
                stale_targets(&trees, &sidecars, &stale_cfg(ttl), NOW.parse().unwrap());
            assert!(due.is_empty(), "ttl_days={ttl}: {due:?}");
            assert_eq!(skipped.len(), 1, "ttl_days={ttl}");
            assert_eq!(skipped[0].why, "no ttl_days configured for mode 'pr'");
        }
    }

    #[test]
    fn f9_no_sidecar_and_a_malformed_created_are_skipped_never_stale() {
        let trees = vec![stale_tree("feat/nometa"), stale_tree("feat/badcreated")];
        let sidecars = vec![stale_sidecar("feat/badcreated", "pr", "not-a-timestamp")];
        let (due, skipped) = stale_targets(&trees, &sidecars, &stale_cfg(3), NOW.parse().unwrap());
        assert!(due.is_empty());
        assert_eq!(skipped.len(), 2);
        assert_eq!(
            skipped[0].why,
            "no readable wt metadata; cannot tell how old it is"
        );
        assert_eq!(
            skipped[1].why,
            "created is not a valid timestamp; cannot tell how old it is"
        );
    }

    // ------------------------------------------------------- merged_targets

    fn merged_tree(leaf: &str, branch: Option<&str>) -> Worktree {
        Worktree {
            path: tree(leaf),
            branch: branch.map(str::to_owned),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn f1_a_fresh_branch_matching_merged_is_spared_not_reaped() {
        let trees = vec![merged_tree("feat/landed", Some("feat/landed"))];
        let merged = set(&["feat/landed"]);
        let history = BTreeMap::from([("feat/landed".to_owned(), BranchHistory::CreationOnly)]);
        let (due, skipped) =
            merged_targets(&trees, &merged, &BTreeMap::new(), &history, "origin/main");
        assert!(due.is_empty(), "{due:?}");
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].why.contains("no commits"), "{}", skipped[0].why);
    }

    #[test]
    fn a_merged_branch_with_real_commits_is_reaped() {
        let trees = vec![merged_tree("feat/landed", Some("feat/landed"))];
        let merged = set(&["feat/landed"]);
        let history = BTreeMap::from([("feat/landed".to_owned(), BranchHistory::HasCommits)]);
        let (due, skipped) =
            merged_targets(&trees, &merged, &BTreeMap::new(), &history, "origin/main");
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].why, "merged into origin/main");
    }

    #[test]
    fn an_unreadable_reflog_spares_too_not_just_creation_only() {
        let trees = vec![merged_tree("feat/unknown", Some("feat/unknown"))];
        let merged = set(&["feat/unknown"]);
        let history = BTreeMap::new(); // no fact gathered: fails closed
        let (due, skipped) =
            merged_targets(&trees, &merged, &BTreeMap::new(), &history, "origin/main");
        assert!(due.is_empty());
        assert_eq!(skipped.len(), 1);
    }

    #[test]
    fn a_squash_merged_branch_whose_upstream_is_gone_is_reaped() {
        let trees = vec![merged_tree("feat/squashed", Some("feat/squashed"))];
        let gone = BTreeMap::from([(
            "feat/squashed".to_owned(),
            "origin/feat/squashed".to_owned(),
        )]);
        let history = BTreeMap::from([("feat/squashed".to_owned(), BranchHistory::HasCommits)]);
        let (due, _) = merged_targets(&trees, &BTreeSet::new(), &gone, &history, "origin/main");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].why, "upstream origin/feat/squashed is gone");
    }

    #[test]
    fn a_gone_upstream_reaps_even_a_creation_only_branch() {
        // F1 gates only the ancestry match: a --guess-remote tree's own
        // reflog only ever shows its creation (the real work came in from
        // the remote branch it tracked, never committed locally), so
        // history must not spare it here.
        let trees = vec![merged_tree("feat/theirs", Some("feat/theirs"))];
        let gone = BTreeMap::from([("feat/theirs".to_owned(), "origin/feat/theirs".to_owned())]);
        let history = BTreeMap::from([("feat/theirs".to_owned(), BranchHistory::CreationOnly)]);
        let (due, skipped) =
            merged_targets(&trees, &BTreeSet::new(), &gone, &history, "origin/main");
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].why, "upstream origin/feat/theirs is gone");
    }

    #[test]
    fn a_branch_never_pushed_is_not_gone() {
        let trees = vec![merged_tree("feat/local-only", Some("feat/local-only"))];
        let (due, skipped) = merged_targets(
            &trees,
            &BTreeSet::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            "origin/main",
        );
        assert!(due.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn gone_to_the_base_itself_is_a_missing_ref_not_a_squash() {
        let trees = vec![merged_tree("feat/x", Some("feat/x"))];
        let gone = BTreeMap::from([("feat/x".to_owned(), "origin/main".to_owned())]);
        let history = BTreeMap::from([("feat/x".to_owned(), BranchHistory::HasCommits)]);
        let (due, skipped) =
            merged_targets(&trees, &BTreeSet::new(), &gone, &history, "origin/main");
        assert!(due.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn test_spares_a_detached_review_tree() {
        let trees = vec![merged_tree("review/pr-1", None)];
        let merged = set(&["review/pr-1"]); // impossible in practice, but proves branch: None wins
        let (due, skipped) = merged_targets(
            &trees,
            &merged,
            &BTreeMap::new(),
            &BTreeMap::new(),
            "origin/main",
        );
        assert!(due.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].why, "detached, so it has no branch to merge");
    }

    // ----------------------------------------------------- TestItemPublishing

    fn item_conf(extra: &str) -> Options {
        let cfg = config::deep_merge(&config::defaults(), &toml::from_str(extra).unwrap());
        config::resolve(&cfg, "item", "ado")
    }

    #[test]
    fn publish_defaults_to_push_only() {
        assert_eq!(publish_steps(&item_conf("")), [Publish::Push]);
        let conf = item_conf("[mode.item]\npush = false\nlink_branch = true\nset_state = \"\"\n");
        assert_eq!(publish_steps(&conf), [Publish::Link]);
        let conf = item_conf("[mode.item]\nlink_branch = true\nset_state = \"Active\"\n");
        assert_eq!(
            publish_steps(&conf),
            [
                Publish::Push,
                Publish::Link,
                Publish::State("Active".to_owned())
            ]
        );
    }

    #[test]
    fn test_push_link_and_state_on_a_clean_run() {
        let steps = [
            Publish::Push,
            Publish::Link,
            Publish::State("Active".to_owned()),
        ];
        let mut ran = Vec::new();
        let out = publish(&steps, |s| {
            ran.push(s.clone());
            provider::Outcome::ok("fine")
        });
        assert_eq!(ran, steps);
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn test_a_failed_push_tells_the_tracker_nothing() {
        let steps = [
            Publish::Push,
            Publish::Link,
            Publish::State("Active".to_owned()),
        ];
        let mut ran = Vec::new();
        let out = publish(&steps, |s| {
            ran.push(s.clone());
            provider::Outcome::failed("push failed: simulated: rejected")
        });
        assert_eq!(
            ran,
            [Publish::Push],
            "linked or activated a branch not on origin"
        );
        assert_eq!(
            out,
            [
                provider::Outcome::failed("push failed: simulated: rejected"),
                provider::Outcome::failed("skipped link/state: branch is not on origin"),
            ]
        );
    }

    #[test]
    fn test_a_forge_without_an_equivalent_is_not_a_failure() {
        let out = publish(&[Publish::Link], |_| provider::Outcome::skipped("no"));
        assert!(!out.iter().any(provider::Outcome::is_failure));
    }

    #[test]
    fn an_item_sidecar_records_title_and_id_but_never_an_empty_id() {
        let spec = CreateSpec {
            mode: "item".to_owned(),
            item_id: Some("21438".to_owned()),
            title: Some(String::new()),
            ..spec("fix/21438-x")
        };
        let meta = |steps: Vec<Step>| {
            steps
                .into_iter()
                .find_map(|s| match s {
                    Step::WriteSidecar { meta, .. } => Some(meta),
                    _ => None,
                })
                .unwrap()
        };
        let m = meta(plan_create(&facts(), &spec, &conf(""), false).unwrap());
        assert_eq!(m.id.as_deref(), Some("21438"));
        assert_eq!(m.title.as_deref(), Some(""));
        let spec = CreateSpec {
            item_id: Some(String::new()),
            ..spec
        };
        let m = meta(plan_create(&facts(), &spec, &conf(""), false).unwrap());
        assert_eq!(m.id, None);
    }
}
