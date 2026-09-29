use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use crate::domain::config;
use crate::domain::herdr;
use crate::domain::naming;
use crate::domain::parse::{self, Meta};
use crate::domain::plan::{self, CreateSpec, Facts, RemoveStep, Step};
use crate::domain::provider;
use crate::domain::select;
use crate::domain::view;
use crate::error::AppError;
use crate::io::cli::{self, ExecOutcome};
use crate::io::git;

/// Everything a verb's plan reads, gathered from `cwd`'s repository. `quiet`
/// silences the corrupt-sidecar warning (F9): the completion listing path
/// (`wt complete`, and the dynamic engine's `ArgValueCompleter`) must never
/// print to stderr, while every other verb still warns.
pub fn gather(cwd: &Path, config_dir: &Path, quiet: bool) -> Result<Facts, AppError> {
    let common_dir = git::common_dir(cwd)?;
    let main_root = parse::main_root_of(&common_dir);
    let meta_dir = parse::meta_dir_of(&common_dir);
    let cfg = load_config(config_dir, &main_root)?;
    // A prunable tree's directory may or may not still be there: checked
    // once here, so every consumer (`ls`, `cd`/`complete`'s exclusion,
    // `rm`, the sweeps) reads the fact instead of probing again - and `rm`
    // never has to spawn git with that tree as cwd to find out, which is
    // exactly the probe that can walk upward into an enclosing repo (see
    // `plan::plan_remove`).
    let worktrees =
        parse::parse_porcelain(&git::run(&["worktree", "list", "--porcelain"], &main_root)?)
            .into_iter()
            .map(|mut w| {
                if w.prunable {
                    w.gone = tree_is_gone(&w.path);
                }
                w
            })
            .collect();
    let branches = git::run(
        &["for-each-ref", "--format=%(refname)", "refs/heads"],
        &main_root,
    )?
    .lines()
    .filter_map(|l| l.strip_prefix("refs/heads/"))
    .map(str::to_owned)
    .collect();
    Ok(Facts {
        provider: provider_of(&main_root, &cfg),
        remote: config::remote_of(&cfg),
        sidecars: read_all_meta(&meta_dir, quiet),
        worktrees,
        branches,
        now: jiff::Timestamp::now(),
        main_root,
        meta_dir,
        config_dir: config_dir.to_path_buf(),
        cfg,
    })
}

/// Reads `<config_dir>/config.toml` and `<main_root>/.wt.toml` (a missing
/// file is fine) and hands their text to `config::load`, which does the
/// actual parse/validate/merge.
fn load_config(config_dir: &Path, main_root: &Path) -> Result<toml::Table, AppError> {
    let mut sources: Vec<(String, String)> = Vec::new();
    for path in [config_dir.join("config.toml"), main_root.join(".wt.toml")] {
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path).map_err(|source| AppError::ConfigRead {
            path: path.clone(),
            source,
        })?;
        sources.push((path.display().to_string(), text));
    }
    let refs: Vec<(&str, &str)> = sources
        .iter()
        .map(|(w, t)| (w.as_str(), t.as_str()))
        .collect();
    config::load(&refs)
}

/// The provider this checkout's remote implies (F19: `ls`/`complete` resolve
/// it themselves, rather than assuming "none", so a `[mode.x.<provider>]`
/// root override is honoured). A missing or unreadable remote is `"none"`,
/// same as the Python tool's `check=False` lookup.
fn provider_of(main_root: &Path, cfg: &toml::Table) -> &'static str {
    parse::provider_of_url(&remote_url(main_root, &config::remote_of(cfg)))
}

fn remote_url(main_root: &Path, remote: &str) -> String {
    stdout_of(&["remote", "get-url", remote], main_root).unwrap_or_default()
}

/// Every readable sidecar under `meta_dir`. An unreadable file is skipped
/// rather than fatal; a JSON value that is not an object is skipped with a
/// warning (F9), unless `quiet` (the completion listing path).
fn read_all_meta(meta_dir: &Path, quiet: bool) -> Vec<(PathBuf, Meta)> {
    let Ok(entries) = std::fs::read_dir(meta_dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        match parse::parse_meta(&text) {
            parse::SidecarRead::Meta(meta) => out.push((file, *meta)),
            parse::SidecarRead::NotObject => {
                if !quiet {
                    eprintln!(
                        "wt: {}: sidecar is not a JSON object, skipping",
                        file.display()
                    );
                }
            }
            parse::SidecarRead::Invalid => {}
        }
    }
    out
}

/// `git status --porcelain`'s outcome, fail-closed (F10): a failed check is
/// its own flag, never read as clean.
fn git_status(path: &Path) -> view::GitStatus {
    match git::try_run(&["status", "--porcelain"], path) {
        Ok(out) if out.success => {
            if out.stdout.trim().is_empty() {
                view::GitStatus::Clean
            } else {
                view::GitStatus::Dirty
            }
        }
        _ => view::GitStatus::Failed,
    }
}

/// Whether `path` itself is gone. git's `prunable` flag alone does not say
/// this: it also fires when only the tree's `.git` file was deleted by hand,
/// leaving everything else on disk (reproduced against real git: deleting
/// just `<tree>/.git` reports the same "prunable gitdir file points to
/// non-existent location" as deleting the whole directory). Only `NotFound`
/// counts as gone; any other error (permission denied, ...) fails closed to
/// "not gone", since that is the safer of the two to be wrong about.
fn tree_is_gone(path: &Path) -> bool {
    matches!(
        path.symlink_metadata(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// The `ls` table's lines.
pub fn ls(facts: &Facts) -> Vec<String> {
    if facts.worktrees.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::with_capacity(facts.worktrees.len());
    for w in &facts.worktrees {
        if config::same_path(&w.path, &facts.main_root) {
            rows.push(view::Row {
                leaf: "(main)".to_owned(),
                ref_: w.branch.clone().unwrap_or_else(|| "-".to_owned()),
                ..view::Row::default()
            });
            continue;
        }
        let meta = select::meta_for_path(&facts.sidecars, &w.path)
            .cloned()
            .unwrap_or_default();
        let leaf = select::leaf_of(&facts.main_root, &facts.cfg, facts.provider, &meta, &w.path);
        let ref_ = w
            .branch
            .clone()
            .unwrap_or_else(|| format!("detached {}", view::short_sha(&w.head)));
        // git's own `prunable` fires whether the directory itself is gone
        // or only its `.git` pointer was deleted, leaving the rest of the
        // tree behind; only the former has nothing left to probe.
        let dirty = if w.prunable {
            if w.gone {
                "missing".to_owned()
            } else {
                "prunable".to_owned()
            }
        } else {
            view::dirty_flags(git_status(&w.path), !meta.exec_failed.is_empty())
        };
        rows.push(view::Row {
            leaf,
            ref_,
            mode: meta.mode.clone().unwrap_or_default(),
            age: view::age_str(meta.created.as_deref(), facts.now),
            dirty,
        });
    }
    view::render_ls(&rows)
}

/// The path `wt cd` prints: the main checkout when `name` is omitted. A
/// prunable tree's directory is gone, so it is never a `cd` target.
pub fn cd(facts: &Facts, name: Option<&str>) -> Result<PathBuf, AppError> {
    let Some(name) = name else {
        return Ok(config::native(&facts.main_root));
    };
    let trees: Vec<_> = linked_trees(facts)
        .into_iter()
        .filter(|w| !w.prunable)
        .collect();
    Ok(config::native(&select::select_one(&trees, name)?.path))
}

/// The names `wt complete` offers. A prunable tree is excluded: its
/// directory is gone, so completing it would only offer a dead end.
pub fn complete(facts: &Facts) -> Vec<String> {
    let mut names = Vec::new();
    for w in &facts.worktrees {
        if config::same_path(&w.path, &facts.main_root) || w.prunable {
            continue;
        }
        let meta = select::meta_for_path(&facts.sidecars, &w.path)
            .cloned()
            .unwrap_or_default();
        names.push(select::leaf_of(
            &facts.main_root,
            &facts.cfg,
            facts.provider,
            &meta,
            &w.path,
        ));
        if let Some(b) = &w.branch {
            names.push(b.clone());
        }
    }
    view::complete_names(names)
}

/// What `wt new`'s flags ask for.
pub struct NewArgs<'a> {
    pub spec: &'a str,
    pub typ: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub no_llm: bool,
    pub open: bool,
    pub run: Option<&'a str>,
}

/// A worktree that exists at the end of a create verb. `partial` is a later
/// step's failure (spec section 4): the path is still printed, the exit is 1.
pub struct Created {
    pub path: PathBuf,
    pub partial: Option<AppError>,
}

pub fn new(facts: &Facts, args: &NewArgs, quiet: bool) -> Result<Created, AppError> {
    let conf = config::resolve(&facts.cfg, "new", facts.provider);
    let branch = if naming::is_branch_spec(args.spec) {
        if conf.fetch {
            fetch(facts);
        }
        args.spec.to_owned()
    } else {
        let chosen = choose_name(facts, args, &conf, quiet, "feat");
        // A fill() template, not a format string.
        #[allow(clippy::literal_string_with_formatting_args)]
        let template = conf.branch.as_deref().unwrap_or("{type}/{slug}");
        naming::fill(template, &[("type", &chosen.typ), ("slug", &chosen.slug)])?
    };
    let spec = CreateSpec {
        mode: "new".to_owned(),
        branch: Some(branch),
        base: resolve_base(facts, &conf)?,
        defer_exec: args.open,
        ..CreateSpec::default()
    };
    let created = create(facts, &spec, &conf, quiet)?;
    if !args.open {
        return Ok(created);
    }
    let branch = spec.branch.as_deref().unwrap_or_default();
    Ok(opened(facts, &spec, &conf, created, branch, args.run))
}

fn create(
    facts: &Facts,
    spec: &CreateSpec,
    conf: &config::Options,
    quiet: bool,
) -> Result<Created, AppError> {
    let target_exists = plan::target_path(facts, spec, conf)?
        .symlink_metadata()
        .is_ok();
    let steps = plan::plan_create(facts, spec, conf, target_exists)?;
    execute(facts, &steps, quiet)
}

/// `--open`: hands the tree to herdr, labelled with `branch`, and its pane
/// the deferred exec steps first, then `--run`, so `--run claude` lands on a
/// tree that is already set up. F12: a failed open is a partial failure, so
/// the path still goes out and the exit is 1; a missing herdr is a skip.
fn opened(
    facts: &Facts,
    spec: &CreateSpec,
    conf: &config::Options,
    mut created: Created,
    branch: &str,
    run: Option<&str>,
) -> Created {
    let outcome = open_outcome(facts, spec, conf, &created.path, branch, run);
    eprintln!("  {}", outcome.detail());
    if matches!(outcome, herdr::Outcome::Failed(_)) {
        created.partial = Some(AppError::PostCreate {
            count: 1,
            path: created.path.clone(),
        });
    }
    created
}

/// `opened`'s herdr call, before it is reported.
fn open_outcome(
    facts: &Facts,
    spec: &CreateSpec,
    conf: &config::Options,
    path: &Path,
    branch: &str,
    run: Option<&str>,
) -> herdr::Outcome {
    let id = spec.item_id.as_deref().unwrap_or_default();
    match plan::exec_cmds(spec, conf, &path.display().to_string()) {
        Ok(cmds) => {
            let deferred = cmds.len();
            let cmds = herdr::with_run(cmds, run);
            let outcome = open_workspace(facts, &spec.mode, path, id, branch, &cmds);
            // Printed even under -q: the tree is not set up as configured.
            if deferred > 0 && matches!(outcome, herdr::Outcome::Skipped(_)) {
                eprintln!("wt: herdr not on PATH; exec steps did not run: {deferred}");
            }
            outcome
        }
        Err(e) => herdr::Outcome::Failed(e.to_string()),
    }
}

/// Opens `path` as a herdr workspace labelled for `mode`.
fn open_workspace(
    facts: &Facts,
    mode: &str,
    path: &Path,
    id: &str,
    branch: &str,
    cmds: &[String],
) -> herdr::Outcome {
    let repo = facts
        .main_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match herdr::label_for(&facts.cfg, mode, repo, id, branch) {
        Ok(label) => cli::herdr_open(&facts.main_root, path, &label, cmds),
        Err(e) => herdr::Outcome::Failed(e.to_string()),
    }
}

/// `wt open`: reopens an existing tree as a herdr workspace, labelled through
/// the mode its sidecar recorded. Nothing on stdout, or the wrapper would
/// follow it into the tree just opened elsewhere. F12: a failed open exits 1.
pub fn open(facts: &Facts, name: &str, run: Option<&str>) -> Result<(), AppError> {
    let trees = linked_trees(facts);
    let tree = select::select_one(&trees, name)?;
    let meta = select::meta_for_path(&facts.sidecars, &tree.path)
        .cloned()
        .unwrap_or_default();
    let r = herdr::reopen(&meta, tree, run);
    let outcome = open_workspace(facts, &r.mode, &tree.path, &r.id, &r.branch, &r.cmds);
    match outcome {
        herdr::Outcome::Failed(detail) => Err(AppError::Cli(detail)),
        done => {
            eprintln!("{}", done.detail());
            Ok(())
        }
    }
}

/// What `wt pr`'s flags ask for.
pub struct PrArgs<'a> {
    pub id: &'a str,
    pub open: bool,
    pub run: Option<&'a str>,
}

/// A detached review tree at PR `id`'s head.
pub fn pr(facts: &Facts, args: &PrArgs, quiet: bool) -> Result<Created, AppError> {
    let forge = provider::from_url(&remote_url(&facts.main_root, &facts.remote))?;
    let conf = config::resolve(&facts.cfg, "pr", facts.provider);
    if conf.fetch {
        fetch(facts);
        if let Some(refspec) = forge.pull_refspec(args.id) {
            fetch_with(facts, &["fetch", &facts.remote, &refspec]);
        }
    }
    let reply = cli::run_json(&forge.pr_argv(args.id)?, &facts.main_root)?;
    let head = forge.pr_head(args.id, &reply)?;
    // A fill() template, not a format string.
    #[allow(clippy::literal_string_with_formatting_args)]
    let template = conf.dirname.as_deref().unwrap_or("review/pr-{id}");
    let dirname = naming::fill(template, &[("id", args.id)])?;
    if !quiet {
        let sha = view::short_sha(&head.sha);
        eprintln!("  PR {}: {} @ {sha}", args.id, head.branch);
    }
    let spec = CreateSpec {
        mode: "pr".to_owned(),
        base: head.sha,
        detach: true,
        dirname: Some(dirname),
        item_id: Some(args.id.to_owned()),
        pr_branch: Some(head.branch),
        defer_exec: args.open,
        ..CreateSpec::default()
    };
    let created = create(facts, &spec, &conf, quiet)?;
    if !args.open {
        return Ok(created);
    }
    let branch = spec.pr_branch.as_deref().unwrap_or_default();
    Ok(opened(facts, &spec, &conf, created, branch, args.run))
}

/// What `wt item`'s flags ask for.
pub struct ItemArgs<'a> {
    pub id: &'a str,
    pub typ: Option<&'a str>,
    pub slug: Option<&'a str>,
    pub no_llm: bool,
    pub open: bool,
    pub run: Option<&'a str>,
}

/// A tree for tracker item `id`, named from its title, then pushed, linked
/// and moved to the configured state, then with `--open` handed to herdr.
/// A post-create failure leaves the tree and is reported as `partial`
/// (spec section 4).
pub fn item(facts: &Facts, args: &ItemArgs, quiet: bool) -> Result<Created, AppError> {
    let forge = provider::from_url(&remote_url(&facts.main_root, &facts.remote))?;
    let conf = config::resolve(&facts.cfg, "item", facts.provider);
    let (branch, title) = name_for_item(facts, &forge, &conf, args, quiet)?;
    let spec = CreateSpec {
        mode: "item".to_owned(),
        branch: Some(branch.clone()),
        base: resolve_base(facts, &conf)?,
        item_id: Some(args.id.to_owned()),
        title: Some(title),
        defer_exec: args.open,
        ..CreateSpec::default()
    };
    let created = create(facts, &spec, &conf, quiet)?;
    let mut outcomes = plan::publish(&plan::publish_steps(&conf), |step| match step {
        plan::Publish::Push => push(facts, &created.path, &branch),
        plan::Publish::Link => link_branch(&forge, args.id, &branch),
        plan::Publish::State(state) => set_state(&forge, args.id, state),
    });
    if args.open {
        outcomes.push(
            match open_outcome(facts, &spec, &conf, &created.path, &branch, args.run) {
                herdr::Outcome::Ok(d) => provider::Outcome::ok(d),
                herdr::Outcome::Skipped(d) => provider::Outcome::skipped(d),
                herdr::Outcome::Failed(d) => provider::Outcome::failed(d),
            },
        );
    }
    // Successes are chatter; failures print even under -q.
    let (failures, notes): (Vec<_>, Vec<_>) = outcomes.iter().partition(|o| o.is_failure());
    for o in notes.iter().filter(|_| !quiet).chain(&failures) {
        eprintln!("  {}", o.detail);
    }
    if failures.is_empty() {
        return Ok(created);
    }
    if let Some(exec) = created.partial {
        eprintln!("wt: {exec}");
    }
    Ok(Created {
        partial: Some(AppError::PostCreate {
            count: failures.len(),
            path: created.path.clone(),
        }),
        path: created.path,
    })
}

/// `(branch, title)` for `wt item`. An earlier run's recorded branch wins
/// (F18), skipping the tracker and the model, so a retry lands on the same
/// tree; its title is then empty, as in the Python tool.
fn name_for_item(
    facts: &Facts,
    forge: &provider::Provider,
    conf: &config::Options,
    args: &ItemArgs,
    quiet: bool,
) -> Result<(String, String), AppError> {
    if let Some(branch) = select::recorded_item_branch(&facts.sidecars, &facts.worktrees, args.id) {
        if !quiet {
            eprintln!("  {}: reusing {branch} from an earlier run", args.id);
        }
        return Ok((branch.to_owned(), String::new()));
    }
    forge.item_refusal().map_or(Ok(()), Err)?;
    let map = provider::type_map(&facts.cfg);
    let item = if let provider::Provider::Ado(Some(ado)) = forge {
        provider::work_item(&cli::az_get(&ado.work_item_fields_url(args.id))?)
    } else {
        let reply = cli::run_json(&provider::gh_issue_argv(args.id), &facts.main_root)?;
        provider::gh_issue(&reply, &map)
    };
    if !quiet {
        eprintln!("  {}: {}  [{}]", args.id, item.title, item.typ);
    }
    let naming_args = NewArgs {
        spec: &item.title,
        typ: args.typ,
        slug: args.slug,
        no_llm: args.no_llm,
        open: args.open,
        run: args.run,
    };
    let fallback = map.get(&item.typ).map_or("feat", String::as_str);
    let chosen = choose_name(facts, &naming_args, conf, quiet, fallback);
    if let Some(refusal) = naming::item_name_refusal(&chosen, conf.push, args.no_llm) {
        return Err(refusal);
    }
    // A fill() template, not a format string.
    #[allow(clippy::literal_string_with_formatting_args)]
    let template = conf.branch.as_deref().unwrap_or("{type}/{id}-{slug}");
    let branch = naming::fill(
        template,
        &[
            ("type", &chosen.typ),
            ("id", args.id),
            ("slug", &chosen.slug),
        ],
    )?;
    Ok((branch, item.title))
}

fn push(facts: &Facts, path: &Path, branch: &str) -> provider::Outcome {
    match git::try_run(&["push", "-u", &facts.remote, branch], path) {
        Ok(out) if out.success => provider::Outcome::ok(format!("pushed {branch}")),
        Ok(out) => {
            provider::Outcome::failed(format!("push failed: {}", provider::excerpt(&out.stderr)))
        }
        Err(e) => provider::Outcome::failed(format!(
            "push failed: {}",
            provider::excerpt(&e.to_string())
        )),
    }
}

/// Adds the branch's `ArtifactLink` to the work item: the repo and project
/// ids first (a GET), then the PATCH.
fn link_branch(forge: &provider::Provider, id: &str, branch: &str) -> provider::Outcome {
    let ado = match forge.write_target("link_branch") {
        Ok(ado) => ado,
        Err(skipped) => return skipped,
    };
    let ids = match cli::az_get(&ado.repo_url()) {
        Ok(reply) => provider::repo_ids(&reply),
        Err(e) => {
            let detail = provider::excerpt(&e.to_string());
            return provider::Outcome::failed(format!("link_branch failed: {detail}"));
        }
    };
    let Some((repo_id, project_id)) = ids else {
        return provider::Outcome::failed("link_branch: az rest returned no repo/project id");
    };
    let patch = provider::link_patch(&project_id, &repo_id, branch);
    provider::link_outcome(id, branch, &cli::az_patch(&ado.work_item_url(id), &patch))
}

fn set_state(forge: &provider::Provider, id: &str, state: &str) -> provider::Outcome {
    let ado = match forge.write_target("set_state") {
        Ok(ado) => ado,
        Err(skipped) => return skipped,
    };
    let sent = cli::az_patch(&ado.work_item_url(id), &provider::state_patch(state));
    provider::state_outcome(id, state, &sent)
}

/// What `wt branch`'s flags ask for.
pub struct BranchArgs<'a> {
    pub name: &'a str,
    pub track: bool,
    pub open: bool,
    pub run: Option<&'a str>,
}

/// A detached review tree at `{remote}/{name}`, or with `--track` a local
/// branch tracking it.
pub fn branch(facts: &Facts, args: &BranchArgs, quiet: bool) -> Result<Created, AppError> {
    let conf = config::resolve(&facts.cfg, "branch", facts.provider);
    if conf.fetch {
        fetch(facts);
    }
    // F16: `detach = false` without --track behaves as --track, rather than
    // branching off a local name that does not exist yet.
    let spec = if args.track || conf.detach == Some(false) {
        CreateSpec {
            branch: Some(args.name.to_owned()),
            base: args.name.to_owned(),
            track: true,
            dirname: Some(args.name.to_owned()),
            ..CreateSpec::default()
        }
    } else {
        // F17: the resolved sha, so a moved remote branch is caught the way
        // a moved PR is.
        let remote_ref = format!("refs/remotes/{}/{}^{{commit}}", facts.remote, args.name);
        let sha = stdout_of(
            &["rev-parse", "--verify", "--quiet", &remote_ref],
            &facts.main_root,
        )
        .ok_or_else(|| AppError::NoRemoteBranch(format!("{}/{}", facts.remote, args.name)))?;
        // A fill() template, not a format string.
        #[allow(clippy::literal_string_with_formatting_args)]
        let template = conf.dirname.as_deref().unwrap_or("review/{slug}");
        CreateSpec {
            base: sha,
            detach: true,
            dirname: Some(naming::fill(template, &[("slug", args.name)])?),
            ..CreateSpec::default()
        }
    };
    let spec = CreateSpec {
        mode: "branch".to_owned(),
        defer_exec: args.open,
        ..spec
    };
    let created = create(facts, &spec, &conf, quiet)?;
    if !args.open {
        return Ok(created);
    }
    Ok(opened(facts, &spec, &conf, created, args.name, args.run))
}

/// Refreshes remote-tracking refs. A failure is reported, not fatal: the
/// create goes on from possibly stale refs.
fn fetch(facts: &Facts) {
    fetch_with(facts, &["fetch", "--prune", &facts.remote]);
}

fn fetch_with(facts: &Facts, args: &[&str]) {
    let detail = match git::try_run(args, &facts.main_root) {
        Ok(out) if out.success => return,
        Ok(out) => out.stderr.trim().to_owned(),
        Err(e) => e.to_string(),
    };
    let excerpt = provider::excerpt(&detail);
    eprintln!("wt: fetch failed, working from possibly stale refs: {excerpt}");
}

/// Picks a branch type and slug for `wt new`'s prose path and `wt item`'s
/// title (`args.spec`): the naming model runs on its own thread while
/// `fetch` runs on this one (the model call dominates wall clock), then the
/// Python tool's precedence decides. Logs the fallback reason unless
/// `quiet`.
fn choose_name(
    facts: &Facts,
    args: &NewArgs,
    conf: &config::Options,
    quiet: bool,
    fallback_type: &str,
) -> naming::ChosenName {
    let naming_cfg = config::naming_of(&facts.cfg);
    let want_model = !(args.no_llm || (args.typ.is_some() && args.slug.is_some()));
    let handle = want_model.then(|| {
        let naming_cfg = naming_cfg.clone();
        let config_dir = facts.config_dir.clone();
        let title = args.spec.to_owned();
        thread::spawn(move || ask_model(&title, &naming_cfg, &config_dir))
    });
    if conf.fetch {
        fetch(facts);
    }
    let (named, why) = handle.map_or((None, String::new()), |h| {
        h.join().unwrap_or_else(|_| (None, String::new()))
    });
    let chosen = naming::chosen_name(
        args.spec,
        (args.typ, args.slug),
        named.as_ref().map(|(t, s)| (t.as_str(), s.as_str())),
        &why,
        want_model && naming_cfg.llm,
        fallback_type,
        &naming_cfg.stopwords,
    );
    if !chosen.fallback_reason.is_empty() && !quiet {
        eprintln!(
            "  (naming fell back to the mechanical slug: {})",
            chosen.fallback_reason
        );
    }
    chosen
}

/// Asks the naming model for a `(type, slug)`, the same shape as the Python
/// tool's `llm_name`: `None` and why, or the model's own answer. Runs from
/// `config_dir` rather than the repo, so a repo's own `CLAUDE.md` cannot
/// turn this into a coding agent that asks questions instead of naming.
fn ask_model(
    title: &str,
    naming_cfg: &config::Naming,
    config_dir: &Path,
) -> (Option<(String, String)>, String) {
    let claude_path = cli::find_claude();
    if let Some(reason) = naming::model_skip_reason(naming_cfg.llm, title, claude_path.is_some()) {
        return (None, reason.to_owned());
    }
    let Some(claude_path) = claude_path else {
        return (None, "claude is not on PATH".to_owned());
    };
    let system = naming_cfg
        .system
        .clone()
        .unwrap_or_else(naming::default_naming_system);
    let argv = naming::claude_argv(
        &claude_path.display().to_string(),
        title,
        &naming_cfg.model,
        &system,
    );
    match cli::run_model(&argv, config_dir, Duration::from_secs(naming_cfg.timeout)) {
        Ok(out) if out.success => match naming::parse_model_reply(&out.stdout) {
            Ok(named) => (Some(named), String::new()),
            Err(reason) => (None, reason),
        },
        Ok(out) => (
            None,
            format!("the naming model exited {}", cli::rc(out.code)),
        ),
        // A timeout, a spawn failure (Python lets that one crash through the
        // Future; wt falls back to the mechanical slug instead), or output
        // held past the drain grace.
        Err(e) => (None, e.to_string()),
    }
}

fn resolve_base(facts: &Facts, conf: &config::Options) -> Result<String, AppError> {
    let default_branch = DefaultBranch::default().get(facts, fallback_branch(conf));
    naming::fill(
        &conf.base,
        &[
            ("default_branch", &default_branch),
            ("remote", &facts.remote),
        ],
    )
}

/// A mode's own `default_branch`: only used when the remote cannot say.
fn fallback_branch(conf: &config::Options) -> &str {
    conf.default_branch.as_deref().unwrap_or("main")
}

/// The remote's default branch, looked up at most once per run however many
/// trees ask: the lookup can cost an `ls-remote`.
#[derive(Default)]
struct DefaultBranch(std::cell::OnceCell<Option<String>>);

impl DefaultBranch {
    /// The remote's answer, else `fallback`. ponytail: the warning names the
    /// first caller's fallback; a later mode with its own gets it silently.
    fn get(&self, facts: &Facts, fallback: &str) -> String {
        self.0
            .get_or_init(|| {
                let found = git::remote_default_branch(&facts.remote, &facts.main_root);
                if found.is_none() {
                    eprintln!(
                        "wt: could not read the remote's default branch; assuming {fallback}"
                    );
                }
                found
            })
            .clone()
            .unwrap_or_else(|| fallback.to_owned())
    }
}

/// What `hook-create` (Claude Code's `WorktreeCreate` hook) reads: the raw
/// stdin payload, `--name`, and this process's pid (the last-resort name,
/// injected here so `naming::hook_name` stays pure and testable).
pub struct HookCreateArgs<'a> {
    pub name: Option<&'a str>,
    pub payload: &'a [u8],
    pub pid: u32,
}

/// `hook-create`: a quiet `new` for Claude Code's `WorktreeCreate` hook. The
/// exec steps run here, not deferred; a re-run with the same session id
/// reuses its own tree through `plan_create`'s existing `Reuse` step.
pub fn hook_create(facts: &Facts, args: &HookCreateArgs) -> Result<Created, AppError> {
    let payload = parse::parse_hook_payload(args.payload)?;
    let name = naming::hook_name(&payload, args.name, args.pid);
    // F23: mode "hook" resolves [mode.new] first, then layers [mode.hook] on
    // top, so hook_type can live in either.
    let conf = config::resolve(&facts.cfg, "hook", facts.provider);
    if conf.fetch {
        fetch(facts);
    }
    let branch = naming::hook_branch_name(
        &name,
        conf.hook_type.as_deref(),
        &naming::stopwords_of(&facts.cfg),
    );
    let spec = CreateSpec {
        mode: "new".to_owned(),
        branch: Some(branch),
        base: resolve_base(facts, &conf)?,
        via: Some("claude".to_owned()),
        ..CreateSpec::default()
    };
    let target_exists = plan::target_path(facts, &spec, &conf)?
        .symlink_metadata()
        .is_ok();
    let steps = plan::plan_create(facts, &spec, &conf, target_exists)?;
    execute(facts, &steps, true)
}

/// `hook-remove`: tears down the tree Claude Code's `WorktreeRemove` hook
/// names, matched by normalised path (F8) against every registered worktree
/// but the main one, under `teardown.mode.hook.force`.
pub fn hook_remove(facts: &Facts, payload: &[u8]) -> Result<(), AppError> {
    let payload = parse::parse_hook_payload(payload)?;
    let target = payload
        .get("worktreePath")
        .or_else(|| payload.get("path"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or(AppError::HookRemoveNoTarget)?;
    // F8: canonicalised where the path exists (8.3 names, links), else
    // compared as given.
    let resolved = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let target_path = resolved(Path::new(target));
    let found = linked_trees(facts)
        .into_iter()
        .find(|w| config::same_path(&resolved(&w.path), &target_path))
        .ok_or_else(|| AppError::HookRemoveNoMatch {
            target: target.to_owned(),
        })?;
    let force = config::resolve_teardown(&facts.cfg, "hook")
        .force
        .unwrap_or(false);
    let policy = RemovePolicy {
        force,
        dry_run: false,
        quiet: true,
    };
    remove_one(facts, &DefaultBranch::default(), &found, policy)?;
    eprintln!(
        "hook-remove: removed {}",
        config::native(&found.path).display()
    );
    Ok(())
}

/// What `wt rm`'s flags ask for. `--stale` and `--merged` conflict with a
/// name and with each other (F20); clap refuses that combination before
/// `gather` even runs, so this never sees more than one of the three.
// Four independent CLI flags, not a state machine: no combination but the
// one clap already refuses is invalid.
#[allow(clippy::struct_excessive_bools)]
pub struct RmArgs<'a> {
    pub name: Option<&'a str>,
    pub stale: bool,
    pub merged: bool,
    pub force: bool,
    pub dry_run: bool,
}

/// `remove_one`'s policy, split out of `RmArgs` so a sweep (and the
/// hook-remove milestone) can hand it a policy of its own rather than a
/// clap-shaped struct with fields it does not use.
#[derive(Debug, Clone, Copy)]
struct RemovePolicy {
    force: bool,
    dry_run: bool,
    quiet: bool,
}

/// `git worktree remove` attempts while the tree is still registered, and
/// the gap between them.
const REMOVE_ATTEMPTS: u32 = 4;
const REMOVE_GAP: std::time::Duration = std::time::Duration::from_millis(400);

pub fn rm(facts: &Facts, args: &RmArgs, quiet: bool) -> Result<(), AppError> {
    // The [teardown] layer: F24 refuses a mode-level fetch, so any mode
    // resolves to the top-level value. Fresh refs let if_merged judge right.
    if config::resolve_teardown(&facts.cfg, "new").fetch {
        fetch(facts);
    }
    let trees = linked_trees(facts);
    let policy = RemovePolicy {
        force: args.force,
        dry_run: args.dry_run,
        quiet,
    };
    let default_branch = DefaultBranch::default();
    if args.stale || args.merged {
        // A prunable tree whose directory is still present cannot be
        // probed or removed at all (its `.git` link is broken); a sweep
        // skips it up front rather than discovering that only when the
        // removal itself refuses partway through.
        let (checkable, mut skipped) = plan::skip_broken_prunable(trees);
        let (due, mut more_skipped) = if args.stale {
            plan::stale_targets(&checkable, &facts.sidecars, &facts.cfg, facts.now)
        } else {
            merged_facts(facts, &default_branch, &checkable)?
        };
        skipped.append(&mut more_skipped);
        return rm_sweep(facts, &default_branch, &due, &skipped, policy);
    }
    let Some(name) = args.name else {
        return Err(AppError::RmWhat);
    };
    let target = select::select_one(&trees, name)?;
    remove_one(facts, &default_branch, target, policy)
}

/// Gathers `--merged`'s facts (the merged set, gone upstreams, and each
/// branch's `BranchHistory`) and hands them to `plan::merged_targets`.
fn merged_facts(
    facts: &Facts,
    default_branch: &DefaultBranch,
    trees: &[parse::Worktree],
) -> Result<(Vec<plan::Due>, Vec<plan::Skipped>), AppError> {
    // One base for the whole sweep: the [defaults] fallback, not any mode's.
    let fallback = facts
        .cfg
        .get("defaults")
        .and_then(|d| d.get("default_branch"))
        .and_then(toml::Value::as_str)
        .unwrap_or("main");
    let base = format!("{}/{}", facts.remote, default_branch.get(facts, fallback));
    let listed = git::try_run(
        &["branch", "--format=%(refname:short)", "--merged", &base],
        &facts.main_root,
    )
    .map_err(|e| AppError::MergedList {
        base: base.clone(),
        detail: provider::excerpt(&e.to_string()),
    })?;
    if !listed.success {
        return Err(AppError::MergedList {
            base,
            detail: provider::excerpt(&listed.stderr),
        });
    }
    let merged: BTreeSet<String> = listed
        .stdout
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let gone = stdout_of(
        &["for-each-ref", parse::GONE_FORMAT, "refs/heads"],
        &facts.main_root,
    )
    .map(|out| parse::gone_upstreams(&out))
    .unwrap_or_default();
    let mut history = BTreeMap::new();
    for w in trees {
        if let Some(branch) = &w.branch {
            history
                .entry(branch.clone())
                .or_insert_with(|| branch_history(&facts.main_root, branch));
        }
    }
    Ok(plan::merged_targets(trees, &merged, &gone, &history, &base))
}

/// F1's fact: what `branch`'s own reflog says about whether anything ever
/// happened to it besides being created. A missing or unreadable reflog
/// fails closed to `Unknown`, so a sweep never treats "could not tell" as
/// "fresh".
fn branch_history(main_root: &Path, branch: &str) -> parse::BranchHistory {
    let ref_arg = format!("refs/heads/{branch}");
    match git::try_run(&["reflog", "show", "--format=%gs", &ref_arg], main_root) {
        Ok(out) if out.success => parse::branch_history(&out.stdout),
        _ => parse::BranchHistory::Unknown,
    }
}

/// The leaf name `rm_sweep`'s messages use: the tree's own directory name,
/// not `select::leaf_of`'s mode-root-relative form (Python's `path.name`).
fn leaf_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// Removes every tree in `due`, reporting `skipped` first. One tree's
/// refusal must not end the sweep: each is caught, and the run reports what
/// could not be removed once every tree has been tried.
fn rm_sweep(
    facts: &Facts,
    default_branch: &DefaultBranch,
    due: &[plan::Due],
    skipped: &[plan::Skipped],
    policy: RemovePolicy,
) -> Result<(), AppError> {
    for s in skipped {
        eprintln!("skipped {}: {}", leaf_name(&s.tree.path), s.why);
    }
    if due.is_empty() {
        eprintln!("nothing to remove");
        return Ok(());
    }
    let mut failed = Vec::new();
    for d in due {
        eprintln!("{}: {}", d.why, d.tree.path.display());
        if let Err(e) = remove_one(facts, default_branch, &d.tree, policy) {
            eprintln!("  {e}");
            failed.push(d.tree.path.clone());
        }
    }
    for path in &failed {
        eprintln!("  left in place: {}", path.display());
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(AppError::SweepFailed {
            failed: failed.len(),
            total: due.len(),
        })
    }
}

/// Every worktree but the main checkout.
fn linked_trees(facts: &Facts) -> Vec<parse::Worktree> {
    facts
        .worktrees
        .iter()
        .filter(|w| !config::same_path(&w.path, &facts.main_root))
        .cloned()
        .collect()
}

/// Tears down one worktree under the policy its sidecar recorded. No dirty
/// probe ever runs against a prunable tree: with its directory gone there is
/// nothing to probe, and with only its `.git` link broken, a probe run
/// there could walk upward into an enclosing repo (e.g. an in-repo
/// `.worktrees` root) and read that repo's status instead, wrongly as
/// clean. `plan::plan_remove` refuses a prunable tree that is not actually
/// gone outright, before any probe or purge, so the placeholder here is
/// never acted on in that case. `plan::plan_remove` finds the sidecar by
/// path either way. A gone tree under `delete_branch = "always"` still has
/// its branch probed, from the main checkout: `branch -D` would otherwise
/// destroy commits that live nowhere else.
fn remove_one(
    facts: &Facts,
    default_branch: &DefaultBranch,
    target: &parse::Worktree,
    policy: RemovePolicy,
) -> Result<(), AppError> {
    // The mode the tree was made under: its fallback default branch is the
    // one its base was built from.
    let mode = select::sidecar_for_path(&facts.sidecars, &config::native(&target.path))
        .and_then(|(_, m)| m.mode.clone())
        .unwrap_or_else(|| "new".to_owned());
    let base = || {
        let conf = config::resolve(&facts.cfg, &mode, facts.provider);
        let branch = default_branch.get(facts, fallback_branch(&conf));
        format!("{}/{branch}", facts.remote)
    };
    let dirty = if !target.prunable {
        dirty_facts(&target.path, base)
    } else if let Some(branch) = &target.branch
        && target.gone
        && config::resolve_teardown(&facts.cfg, &mode).delete_branch == "always"
    {
        gone_branch_facts(&facts.main_root, branch, base)
    } else {
        plan::DirtyFacts::nothing_to_probe()
    };
    let removal = plan::plan_remove(facts, target, &dirty, policy.force, policy.dry_run)?;
    if policy.dry_run {
        for line in view::describe_removal(&removal) {
            eprintln!("{line}");
        }
        return Ok(());
    }
    let steps = plan::removal_steps(&removal, policy.force)?;
    // Windows will not delete a process's cwd, and wt may have been started
    // inside the tree it is removing. A shell sitting there still holds it.
    let _ = std::env::set_current_dir(&facts.main_root);
    let mut sidecar_left = None;
    for step in &steps {
        match step {
            RemoveStep::Purge { tree, rels } => purge_copied(tree, rels)?,
            RemoveStep::RemoveCheckout { path, force } => {
                remove_checkout(&facts.main_root, path, *force)?;
            }
            RemoveStep::DeleteBranch { branch, force } => {
                delete_branch(&facts.main_root, branch, *force, policy.quiet);
            }
            // The tree is gone by now: finish the teardown, then fail.
            RemoveStep::DropSidecar { file } => match std::fs::remove_file(file) {
                Err(source) if source.kind() != std::io::ErrorKind::NotFound => {
                    sidecar_left = Some(AppError::SidecarLeft {
                        path: config::native(file),
                        source,
                    });
                }
                _ => {}
            },
            RemoveStep::Prune => {
                let _ = git::try_run(&["worktree", "prune"], &facts.main_root);
            }
        }
    }
    if !policy.quiet {
        eprintln!("removed: {}", removal.path.display());
    }
    drop_empty_dir(&removal.path);
    sidecar_left.map_or(Ok(()), Err)
}

/// Runs the dirty probes in `path`; `plan::dirty_reasons` decides what they
/// mean. `base` is `{remote}/{default branch}`, only looked up for a branch.
fn dirty_facts(path: &Path, base: impl FnOnce() -> String) -> plan::DirtyFacts {
    let status = match git::try_run(&["status", "--porcelain"], path) {
        Ok(out) if out.success => Ok(!out.stdout.trim().is_empty()),
        Ok(out) => Err(provider::excerpt(&out.stderr)),
        Err(e) => Err(provider::excerpt(&e.to_string())),
    };
    let head = stdout_of(&["rev-parse", "--abbrev-ref", "HEAD"], path);
    let mut dirty = plan::DirtyFacts {
        status,
        head: head.clone(),
        ..plan::DirtyFacts::nothing_to_probe()
    };
    if let Some(branch) = head.filter(|h| h != "HEAD") {
        branch_facts(&mut dirty, path, &branch, base(), "HEAD");
    }
    dirty
}

/// A gone tree's branch, probed from the main checkout: there is no status
/// left to read, but its commits live on in `refs/heads/<branch>`.
fn gone_branch_facts(
    main_root: &Path,
    branch: &str,
    base: impl FnOnce() -> String,
) -> plan::DirtyFacts {
    let mut dirty = plan::DirtyFacts {
        head: Some(branch.to_owned()),
        ..plan::DirtyFacts::nothing_to_probe()
    };
    let tip = format!("refs/heads/{branch}");
    branch_facts(&mut dirty, main_root, branch, base(), &tip);
    dirty
}

/// The upstream, gone and ahead probes for `branch`, whose tip is `tip`.
fn branch_facts(dirty: &mut plan::DirtyFacts, cwd: &Path, branch: &str, base: String, tip: &str) {
    let upstream = format!("{branch}@{{u}}");
    let upstream = [
        "rev-parse",
        "--abbrev-ref",
        "--symbolic-full-name",
        &upstream,
    ];
    dirty.upstream = stdout_of(&upstream, cwd).unwrap_or_default();
    dirty.gone = stdout_of(&["for-each-ref", parse::GONE_FORMAT, "refs/heads"], cwd)
        .and_then(|out| parse::gone_upstreams(&out).remove(branch));
    dirty.base = base;
    if let Some(lhs) = plan::count_base(dirty) {
        let range = format!("{lhs}..{tip}");
        dirty.ahead = stdout_of(&["rev-list", "--count", &range], cwd);
    }
}

/// A git call's trimmed stdout, or `None` when it failed.
fn stdout_of(args: &[&str], cwd: &Path) -> Option<String> {
    git::try_run(args, cwd)
        .ok()
        .filter(|out| out.success)
        .map(|out| out.stdout.trim().to_owned())
}

/// Deletes the copied entries. The plan kept only those inside the tree by
/// path arithmetic; a symlinked directory on the way could still lead out,
/// so the entry's parent must resolve inside the tree too. The entry itself
/// is never followed: a link is removed, not its target. A missing entry is
/// not an error.
fn purge_copied(tree: &Path, rels: &[String]) -> Result<(), AppError> {
    let real_tree = match std::fs::canonicalize(tree) {
        Ok(real) => real,
        // Gone already: nothing to purge, and `git worktree remove` accepts it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(AppError::Purge {
                path: tree.to_path_buf(),
                source,
            });
        }
    };
    for rel in rels {
        let target = tree.join(rel);
        let Ok(meta) = target.symlink_metadata() else {
            continue;
        };
        let inside = target
            .parent()
            .and_then(|p| std::fs::canonicalize(p).ok())
            .is_some_and(|p| config::strip_under(&p, &real_tree).is_some());
        if !inside {
            continue;
        }
        let removed = if meta.is_dir() || (meta.is_symlink() && target.is_dir()) {
            std::fs::remove_dir_all(&target)
        } else {
            std::fs::remove_file(&target)
        };
        removed.map_err(|source| AppError::Purge {
            path: target,
            source,
        })?;
    }
    Ok(())
}

/// Whether git still lists `path` (or anything under it) as a worktree.
/// Fails closed: a listing git cannot give counts as still registered.
fn registered(main_root: &Path, path: &Path) -> bool {
    git::try_run(&["worktree", "list", "--porcelain"], main_root)
        .ok()
        .filter(|out| out.success)
        .is_none_or(|out| {
            parse::parse_porcelain(&out.stdout)
                .iter()
                .any(|w| config::strip_under(&w.path, path).is_some())
        })
}

/// Deletes the checkout, judged by registration rather than the exit code:
/// git deregisters the tree even when, on Windows, it then cannot unlink a
/// directory some process sits in. Retried while still registered, since a
/// pane's `cd` may not have landed yet. The first error is the one reported:
/// a retry on an already-removed tree only says "is not a working tree".
fn remove_checkout(main_root: &Path, path: &Path, force: bool) -> Result<(), AppError> {
    let left = cli::release_panes(main_root, path);
    let path_arg = path.display().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&path_arg);
    let succeeded = |r: &Result<git::Output, AppError>| r.as_ref().is_ok_and(|o| o.success);
    let first = git::try_run(&args, main_root);
    let mut ok = succeeded(&first);
    for _ in 1..REMOVE_ATTEMPTS {
        if ok || !registered(main_root, path) {
            break;
        }
        std::thread::sleep(REMOVE_GAP);
        ok = succeeded(&git::try_run(&args, main_root));
    }
    if !registered(main_root, path) {
        return Ok(());
    }
    let first = match first {
        Ok(out) => provider::excerpt(&out.stderr),
        Err(e) => provider::excerpt(&e.to_string()),
    };
    // A sharing violation names no culprit: say which panes herdr last had
    // in there and could not be moved, stale as that reading can be.
    let held = if left.is_empty() {
        String::new()
    } else {
        format!("; herdr last had {} in it", left.join(", "))
    };
    Err(AppError::RemoveFailed {
        path: path.to_path_buf(),
        detail: format!("{first}{held}"),
    })
}

/// A refusal from git is information, not an error: `-d` declines to drop
/// an unmerged branch, which is the point.
fn delete_branch(main_root: &Path, branch: &str, force: bool, quiet: bool) {
    let flag = if force { "-D" } else { "-d" };
    let detail = match git::try_run(&["branch", flag, branch], main_root) {
        Ok(out) if out.success => return,
        Ok(out) => out.stderr.trim().to_owned(),
        Err(e) => e.to_string(),
    };
    if !quiet {
        eprintln!("  kept branch {branch}: {detail}");
    }
}

/// The emptied directory git could not unlink, worth one more try now that
/// the teardown is done. It is no longer a worktree, so `wt rm` will not
/// match it again: say so if it is still held.
fn drop_empty_dir(path: &Path) {
    if path.symlink_metadata().is_err() {
        return;
    }
    if let Err(e) = std::fs::remove_dir(path) {
        eprintln!(
            "wt: the empty directory is still held, remove it later: {} ({e})",
            path.display()
        );
    }
}

/// A completed step a rollback has to undo. Seeded files need no entry of
/// their own: removing the worktree takes them with it.
enum Done {
    Branch(String),
    Worktree(PathBuf),
    Sidecar(PathBuf),
}

/// Runs `steps` in order. A failure after the tree exists rolls back exactly
/// what was done, except a non-strict exec failure, which is recorded in the
/// sidecar and reported as `partial`.
fn execute(facts: &Facts, steps: &[Step], quiet: bool) -> Result<Created, AppError> {
    let mut done: Vec<Done> = Vec::new();
    let mut made: Option<PathBuf> = None;
    let mut seeded: Vec<String> = Vec::new();
    let mut sidecar: Option<(PathBuf, Meta)> = None;
    let mut partial = None;
    for step in steps {
        let result = match step {
            Step::Reuse { path } => {
                if !quiet {
                    eprintln!("exists: {}", path.display());
                }
                return Ok(Created {
                    path: path.clone(),
                    partial: None,
                });
            }
            Step::AddWorktree { path, arm } => {
                // Recorded before the add: `worktree add -b` can create the
                // branch and still fail, and the branch must not outlive it.
                if let Some(branch) = arm.created_branch() {
                    done.push(Done::Branch(branch.to_owned()));
                }
                add_worktree(&facts.main_root, path, arm).map(|()| {
                    done.push(Done::Worktree(path.clone()));
                    made = Some(path.clone());
                })
            }
            Step::Seed {
                from,
                to,
                copy,
                symlink,
            } => seed(from, to, copy, symlink).map(|entries| seeded = entries),
            Step::WriteSidecar { file, meta } => {
                let meta = Meta {
                    copied: seeded.clone(),
                    ..meta.clone()
                };
                let written = write_sidecar(file, &meta);
                if written.is_ok() {
                    done.push(Done::Sidecar(file.clone()));
                    sidecar = Some((file.clone(), meta));
                }
                written
            }
            Step::Exec {
                cmds,
                shell,
                timeout,
                strict,
            } => {
                let path = made.clone().unwrap_or_default();
                let shell = shell.clone().unwrap_or_else(cli::default_shell);
                let failed = run_exec(&shell, cmds, &path, *timeout, quiet);
                record_exec_failures(&path, &failed, *strict, &mut sidecar)
                    .map(|failure| partial = failure)
            }
            Step::DeferExec { strict } => {
                if *strict {
                    eprintln!(
                        "wt: exec runs in the new workspace, so exec_strict cannot roll this tree back"
                    );
                }
                Ok(())
            }
        };
        if let Err(e) = result {
            roll_back(&facts.main_root, &done);
            return Err(e);
        }
    }
    let path = made.unwrap_or_default();
    if !quiet {
        eprintln!("created: {}", path.display());
    }
    Ok(Created { path, partial })
}

/// A non-strict failure is recorded in the sidecar and handed back as the
/// partial failure; a strict one is an error, so the caller rolls back.
fn record_exec_failures(
    path: &Path,
    failed: &[String],
    strict: bool,
    sidecar: &mut Option<(PathBuf, Meta)>,
) -> Result<Option<AppError>, AppError> {
    let Some(first) = failed.first() else {
        return Ok(None);
    };
    if let Some((file, meta)) = sidecar {
        meta.exec_failed = failed.to_vec();
        if let Err(e) = write_sidecar(file, meta) {
            eprintln!("wt: {e}");
        }
    }
    if !strict {
        return Ok(Some(AppError::ExecPartial {
            count: failed.len(),
            path: path.to_path_buf(),
        }));
    }
    eprintln!(
        "  {} exec step(s) failed; the worktree is at {}",
        failed.len(),
        path.display()
    );
    Err(AppError::ExecStrict {
        path: path.to_path_buf(),
        first: first.clone(),
    })
}

fn add_worktree(main_root: &Path, path: &Path, arm: &plan::AddArm) -> Result<(), AppError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AppError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let args = arm.git_args(&path.display().to_string());
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    git::run(&args, main_root).map(drop)
}

/// Copies and links the configured entries from the main checkout into the
/// new tree. Returns what it placed there (F7: links too), so teardown knows
/// what to purge. A missing source is skipped.
fn seed(
    from: &Path,
    to: &Path,
    copy: &[String],
    symlink: &[String],
) -> Result<Vec<String>, AppError> {
    let mut placed = Vec::new();
    for rel in copy {
        let src = from.join(rel);
        if !src.exists() {
            continue;
        }
        copy_entry(&src, &to.join(rel)).map_err(|source| AppError::Seed {
            rel: rel.clone(),
            source,
        })?;
        placed.push(rel.clone());
    }
    for rel in symlink {
        let (src, dst) = (from.join(rel), to.join(rel));
        if !src.exists() || dst.symlink_metadata().is_ok() {
            continue;
        }
        link_entry(&src, &dst).map_err(|source| AppError::Seed {
            rel: rel.clone(),
            source,
        })?;
        placed.push(rel.clone());
    }
    Ok(placed)
}

fn copy_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !src.is_dir() {
        return std::fs::copy(src, dst).map(drop);
    }
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let (from, to) = (entry.path(), dst.join(entry.file_name()));
        // A linked directory is copied as the link: following one that points
        // back up would recurse without end.
        if entry.file_type()?.is_symlink() && from.is_dir() {
            let target = std::fs::read_link(&from)?;
            #[cfg(windows)]
            let linked = std::os::windows::fs::symlink_dir(&target, &to);
            #[cfg(not(windows))]
            let linked = std::os::unix::fs::symlink(&target, &to);
            match linked {
                // Windows without Developer Mode cannot make the link.
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    copy_linked_dir(&from, &to)?;
                }
                linked => linked?,
            }
        } else {
            copy_entry(&from, &to)?;
        }
    }
    Ok(())
}

/// Copies what the directory link `link` points at, as Python's copytree
/// did, unless it points back up to a directory that holds the link.
/// ponytail: a loop through two links outside the tree (A -> B -> A) still
/// recurses without end; track the canonical directories visited if one
/// turns up.
fn copy_linked_dir(link: &Path, to: &Path) -> std::io::Result<()> {
    let target = std::fs::canonicalize(link)?;
    if let Some(parent) = link.parent()
        && std::fs::canonicalize(parent)?.starts_with(&target)
    {
        eprintln!(
            "wt: skipped {}: it links to a directory that holds it",
            link.display()
        );
        return Ok(());
    }
    copy_entry(link, to)
}

fn link_entry(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    #[cfg(windows)]
    {
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, dst)
        } else {
            std::os::windows::fs::symlink_file(src, dst)
        }
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(src, dst)
    }
}

fn write_sidecar(file: &Path, meta: &Meta) -> Result<(), AppError> {
    let write = || {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(file, parse::sidecar_json(meta)?)
    };
    write().map_err(|source| AppError::Write {
        path: file.to_path_buf(),
        source,
    })
}

/// Runs each command, logging as the Python tool does. Returns the ones
/// that failed.
fn run_exec(
    shell: &[String],
    cmds: &[String],
    cwd: &Path,
    timeout: std::time::Duration,
    quiet: bool,
) -> Vec<String> {
    let mut failed = Vec::new();
    for cmd in cmds {
        if !quiet {
            eprintln!("  exec: {cmd}");
        }
        match cli::run_exec(shell, cmd, cwd, timeout) {
            ExecOutcome::Ok => continue,
            ExecOutcome::TimedOut => {
                eprintln!("wt: exec timed out after {}s: {cmd}", timeout.as_secs());
            }
            ExecOutcome::Failed(code) => {
                eprintln!("wt: exec failed (rc={}): {cmd}", cli::rc(code));
            }
            ExecOutcome::Spawn(e) => {
                eprintln!("wt: exec failed ({e}): {cmd}");
            }
        }
        failed.push(cmd.clone());
    }
    failed
}

/// Undoes what a create that did not finish had done, newest first.
/// Best-effort: another error is already on its way up, and must not be
/// masked.
fn roll_back(main_root: &Path, done: &[Done]) {
    if let Some(path) = done.iter().find_map(|d| match d {
        Done::Worktree(p) => Some(p),
        _ => None,
    }) {
        eprintln!("wt: rolling back {}", path.display());
    }
    for step in done.iter().rev() {
        match step {
            Done::Sidecar(file) => {
                let _ = std::fs::remove_file(file);
            }
            Done::Worktree(path) => {
                let path = path.display().to_string();
                let _ = git::try_run(&["worktree", "remove", "--force", &path], main_root);
            }
            Done::Branch(branch) => {
                let _ = git::try_run(&["branch", "-D", branch], main_root);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory link: a junction on Windows, which needs no privilege,
    /// so the test runs without Developer Mode.
    fn dir_link(target: &Path, link: &Path) {
        #[cfg(windows)]
        {
            let made = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(made.status.success(), "{made:?}");
        }
        #[cfg(not(windows))]
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    /// A directory link that points back up is never followed: it is copied
    /// as the link, or, where the link cannot be made, skipped.
    #[test]
    fn a_directory_link_inside_a_copied_dir_is_not_followed() {
        let root = std::env::temp_dir().join(format!("wt-copy-loop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), "a").unwrap();
        dir_link(&src, &src.join("loop"));
        let dst = root.join("dst");
        let copied = copy_entry(&src, &dst);
        let link = dst.join("loop").symlink_metadata();
        let a = std::fs::read_to_string(dst.join("a.txt"));
        let _ = std::fs::remove_dir_all(&root);
        copied.unwrap();
        assert_eq!(a.unwrap(), "a");
        assert!(link.map_or(true, |m| m.is_symlink()), "loop was followed");
    }

    /// Where the link cannot be made, what it points at is copied.
    #[test]
    fn a_denied_directory_link_to_outside_is_copied() {
        let root = std::env::temp_dir().join(format!("wt-copy-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, outside) = (root.join("src"), root.join("outside"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("b.txt"), "b").unwrap();
        dir_link(&outside, &src.join("out"));
        let to = root.join("dst").join("out");
        let copied = copy_linked_dir(&src.join("out"), &to);
        let b = std::fs::read_to_string(to.join("b.txt"));
        let is_link = to.symlink_metadata().map(|m| m.is_symlink());
        let _ = std::fs::remove_dir_all(&root);
        copied.unwrap();
        assert_eq!(b.unwrap(), "b");
        assert!(!is_link.unwrap(), "copied, not linked");
    }

    /// Where the link cannot be made and it points back up, copying would
    /// never end: it is skipped.
    #[test]
    fn a_denied_directory_link_to_its_own_parent_is_skipped() {
        let root = std::env::temp_dir().join(format!("wt-copy-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        dir_link(&src, &src.join("loop"));
        let to = root.join("dst").join("loop");
        let copied = copy_linked_dir(&src.join("loop"), &to);
        let left = to.symlink_metadata();
        let _ = std::fs::remove_dir_all(&root);
        copied.unwrap();
        assert!(left.is_err(), "loop was copied");
    }

    #[test]
    fn a_tree_that_is_already_gone_has_nothing_to_purge() {
        let gone = std::env::temp_dir().join(format!("wt-purge-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&gone);
        purge_copied(&gone, &[".env".to_owned()]).unwrap();
    }

    // ------------------------------------------------------- prunable trees

    /// `Facts` with the main tree plus whatever `extra` worktrees a test
    /// wants to add; enough for `ls`/`cd`/`complete`, which never spawn git.
    fn wt_facts(extra: Vec<parse::Worktree>) -> Facts {
        let main = parse::Worktree {
            path: PathBuf::from("/r/repo"),
            branch: Some("main".to_owned()),
            head: "abc".to_owned(),
            prunable: false,
            gone: false,
        };
        let mut worktrees = vec![main];
        worktrees.extend(extra);
        Facts {
            main_root: PathBuf::from("/r/repo"),
            meta_dir: PathBuf::from("/r/repo/.git/wt"),
            config_dir: PathBuf::from("/r/config"),
            cfg: toml::Table::new(),
            provider: "none",
            remote: "origin".to_owned(),
            worktrees,
            sidecars: vec![],
            branches: vec![],
            now: jiff::Timestamp::now(),
        }
    }

    fn prunable_tree() -> parse::Worktree {
        parse::Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/gone"),
            branch: Some("feat/gone".to_owned()),
            head: "abc".to_owned(),
            prunable: true,
            gone: true,
        }
    }

    #[test]
    fn ls_flags_a_prunable_tree_as_missing_without_probing_it() {
        let facts = wt_facts(vec![prunable_tree()]);
        let lines = ls(&facts);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("feat/gone") && l.ends_with("missing")),
            "{lines:?}"
        );
    }

    #[test]
    fn ls_flags_a_present_prunable_tree_as_prunable_not_missing() {
        // git's `prunable` also fires when only the tree's own `.git` file
        // was deleted, leaving the directory itself on disk (`gone: false`,
        // as `gather` would have found it).
        let facts = wt_facts(vec![parse::Worktree {
            path: PathBuf::from("/r/repo.worktrees/feat/present"),
            branch: Some("feat/present".to_owned()),
            head: "abc".to_owned(),
            prunable: true,
            gone: false,
        }]);
        let lines = ls(&facts);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("feat/present") && l.ends_with("prunable")),
            "{lines:?}"
        );
    }

    #[test]
    fn tree_is_gone_is_true_only_for_not_found() {
        let missing = std::env::temp_dir().join(format!("wt-gone-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&missing);
        assert!(tree_is_gone(&missing));
        assert!(!tree_is_gone(&std::env::temp_dir()));
    }

    #[test]
    fn cd_excludes_a_prunable_tree() {
        let facts = wt_facts(vec![prunable_tree()]);
        assert!(matches!(
            cd(&facts, Some("feat/gone")),
            Err(AppError::NoWorktreeMatching(_))
        ));
    }

    #[test]
    fn complete_excludes_a_prunable_tree() {
        let facts = wt_facts(vec![prunable_tree()]);
        assert!(!complete(&facts).iter().any(|n| n.contains("gone")));
    }
}
