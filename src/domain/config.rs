use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use crate::domain::naming;
use crate::error::AppError;

/// Layer zero of the merge: the values wt has even with no config.toml at all.
/// A literal, not a hand-built map, so it reads the same as `config.toml` itself.
const DEFAULTS_TOML: &str = r#"
[defaults]
root = "../{repo}.worktrees"
fetch = true
base = "origin/{default_branch}"
branch = "{type}/{id}-{slug}"
copy = []
symlink = []
exec = []
exec_strict = false
exec_timeout = 600

[mode.new]
branch = "{type}/{slug}"

[mode.branch]
detach = true
dirname = "review/{slug}"
copy = []

[mode.pr]
dirname = "review/pr-{id}"
detach = true
copy = []

[teardown]
require_clean = true
delete_branch = "if_merged"
purge_copied = true
prune = true
fetch = false

[teardown.mode.hook]
force = false

[naming]
llm = false
model = "haiku"
timeout = 60
"#;

/// Parsing a literal we wrote above cannot fail in practice; a broken literal
/// would show up as every default-dependent test failing, not as a panic.
#[must_use]
pub fn defaults() -> Table {
    toml::from_str(DEFAULTS_TOML).unwrap_or_default()
}

/// Tables merge; arrays and scalars replace. Documented behaviour, not an
/// accident: a repo's `copy = [...]` must override, never append.
#[must_use]
pub fn deep_merge(base: &Table, over: &Table) -> Table {
    let mut out = base.clone();
    for (k, v) in over {
        let merged = match (out.get(k), v) {
            (Some(Value::Table(bt)), Value::Table(ot)) => Value::Table(deep_merge(bt, ot)),
            _ => v.clone(),
        };
        out.insert(k.clone(), merged);
    }
    out
}

/// Create options: `[defaults]`, then the scalars of `[mode.<mode>]`, then
/// `[mode.<mode>.<provider>]`. A nested table is a namespace, not an option,
/// so it is skipped by shape.
///
/// Mode `"hook"` also inherits `[mode.new]` and `[mode.new.<provider>]` first
/// (Python's hook-create actually resolves mode `"new"`), then layers
/// `[mode.hook]` and `[mode.hook.<provider>]` on top, so `hook_type` can live
/// in either (F23).
#[must_use]
pub fn resolve(cfg: &Table, mode: &str, provider: &str) -> Options {
    let defaults = sub_table(cfg, "defaults");
    let mut merged: Table = defaults.cloned().unwrap_or_default();
    let mode_table = sub_table(cfg, "mode");
    if mode == "hook" {
        let new_mode = mode_table.and_then(|m| sub_table(m, "new"));
        merge_scalars(&mut merged, new_mode);
        merge_scalars(&mut merged, new_mode.and_then(|m| sub_table(m, provider)));
    }
    let this_mode = mode_table.and_then(|m| sub_table(m, mode));
    merge_scalars(&mut merged, this_mode);
    merge_scalars(&mut merged, this_mode.and_then(|m| sub_table(m, provider)));
    Options::from_table(&merged)
}

/// Teardown options: the scalars of `[teardown]` (excluding `mode`), then
/// `[teardown.mode.<mode>]`.
#[must_use]
pub fn resolve_teardown(cfg: &Table, mode: &str) -> Teardown {
    let teardown = sub_table(cfg, "teardown");
    let mut merged = Table::new();
    if let Some(t) = teardown {
        for (k, v) in t {
            if k != "mode" && !matches!(v, Value::Table(_)) {
                merged.insert(k.clone(), v.clone());
            }
        }
    }
    let this_mode = teardown
        .and_then(|t| sub_table(t, "mode"))
        .and_then(|m| sub_table(m, mode));
    merge_scalars(&mut merged, this_mode);
    Teardown::from_table(&merged)
}

/// Parses, validates (per file) and deep-merges `files` over `defaults()`.
/// Each `(where, text)` pair is one file's display label and its contents,
/// lowest layer first (global config, then repo `.wt.toml`); the actual
/// reads, and deciding which files exist, are the caller's job (IO belongs in
/// `main.rs`/`run.rs`, not here). A file's problems are reported against its
/// own `where` label.
pub fn load(files: &[(&str, &str)]) -> Result<Table, AppError> {
    let mut cfg = defaults();
    for (where_, text) in files {
        let parsed: Table = toml::from_str(text).map_err(|source| AppError::ConfigParse {
            file: (*where_).to_owned(),
            source,
        })?;
        validate(&parsed, where_)?;
        cfg = deep_merge(&cfg, &parsed);
    }
    Ok(cfg)
}

/// A mode's resolved create options. Fields DEFAULTS always supplies are
/// plain values (`resolve` falls back to the same literal when a caller's
/// table omits them, e.g. in a unit test); fields with no unconditional
/// default, or whose fallback is verb-specific, are `Option`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub root: String,
    pub remote: Option<String>,
    pub default_branch: Option<String>,
    pub fetch: bool,
    pub base: String,
    pub branch: Option<String>,
    pub dirname: Option<String>,
    pub detach: Option<bool>,
    pub copy: Vec<String>,
    pub symlink: Vec<String>,
    pub exec: Vec<String>,
    pub exec_strict: bool,
    pub exec_timeout: i64,
    pub shell: Option<Vec<String>>,
    pub push: bool,
    pub link_branch: Option<bool>,
    pub set_state: Option<String>,
    pub hook_type: Option<String>,
}

impl Options {
    fn from_table(t: &Table) -> Self {
        Self {
            root: str_of(t, "root").unwrap_or_else(|| "../{repo}.worktrees".to_owned()),
            remote: str_of(t, "remote"),
            default_branch: str_of(t, "default_branch"),
            fetch: bool_of(t, "fetch").unwrap_or(true),
            base: str_of(t, "base").unwrap_or_else(|| "origin/{default_branch}".to_owned()),
            branch: str_of(t, "branch"),
            dirname: str_of(t, "dirname"),
            detach: bool_of(t, "detach"),
            copy: list_of(t, "copy").unwrap_or_default(),
            symlink: list_of(t, "symlink").unwrap_or_default(),
            exec: list_of(t, "exec").unwrap_or_default(),
            exec_strict: bool_of(t, "exec_strict").unwrap_or(false),
            exec_timeout: int_of(t, "exec_timeout").unwrap_or(600),
            shell: list_of(t, "shell"),
            push: bool_of(t, "push").unwrap_or(true),
            link_branch: bool_of(t, "link_branch"),
            set_state: str_of(t, "set_state"),
            hook_type: str_of(t, "hook_type"),
        }
    }
}

/// A mode's resolved teardown policy.
// Four independent on/off settings from the config file, not a state
// machine: no combination of them is invalid.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Teardown {
    pub fetch: bool,
    pub require_clean: bool,
    pub delete_branch: String,
    pub purge_copied: bool,
    pub prune: bool,
    pub force: Option<bool>,
    pub ttl_days: Option<i64>,
}

impl Teardown {
    fn from_table(t: &Table) -> Self {
        Self {
            fetch: bool_of(t, "fetch").unwrap_or(false),
            require_clean: bool_of(t, "require_clean").unwrap_or(true),
            delete_branch: str_of(t, "delete_branch").unwrap_or_else(|| "if_merged".to_owned()),
            purge_copied: bool_of(t, "purge_copied").unwrap_or(true),
            prune: bool_of(t, "prune").unwrap_or(true),
            force: bool_of(t, "force"),
            ttl_days: int_of(t, "ttl_days"),
        }
    }
}

/// `[naming]`, resolved: whether the model runs, which one, its timeout in
/// seconds, an optional system-prompt override, and stopwords. Mirrors
/// `DEFAULTS_TOML`'s `[naming]` layer (`llm = false`, `model = "haiku"`,
/// `timeout = 60`); `system` and `stopwords` have no unconditional default,
/// same as `Options`'s `Option` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Naming {
    pub llm: bool,
    pub model: String,
    pub timeout: u64,
    pub system: Option<String>,
    pub stopwords: Vec<String>,
}

/// Reads `[naming]` from the merged config table.
#[must_use]
pub fn naming_of(cfg: &Table) -> Naming {
    let naming = sub_table(cfg, "naming");
    Naming {
        llm: naming.and_then(|n| bool_of(n, "llm")).unwrap_or(false),
        model: naming
            .and_then(|n| str_of(n, "model"))
            .unwrap_or_else(|| "haiku".to_owned()),
        timeout: naming
            .and_then(|n| int_of(n, "timeout"))
            .and_then(|t| u64::try_from(t).ok())
            .unwrap_or(60),
        system: naming.and_then(|n| str_of(n, "system")),
        stopwords: naming::stopwords_of(cfg),
    }
}

/// `[defaults] remote`, or "origin" when unset.
#[must_use]
pub fn remote_of(cfg: &Table) -> String {
    sub_table(cfg, "defaults")
        .and_then(|d| str_of(d, "remote"))
        .unwrap_or_else(|| "origin".to_owned())
}

/// Where this mode's worktrees live: `opts.root` with `{repo}` filled in,
/// joined onto `root` and lexically normalised. No filesystem access (the
/// tree need not exist yet), so a `..` component is collapsed by pure path
/// arithmetic rather than `fs::canonicalize` - close enough to Python's
/// `Path.resolve()` for a path this is only ever compared against another
/// lexically-built path.
#[must_use]
pub fn wt_root_of(root: &Path, opts: &Options) -> PathBuf {
    let repo = root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    // Not a format string: "{repo}" is the literal template placeholder
    // `.replace()` substitutes, not an interpolation the lint mistakes it for.
    #[allow(clippy::literal_string_with_formatting_args)]
    let filled = opts.root.replace("{repo}", repo);
    normalize_lexical(&root.join(filled))
}

/// The one home of path identity. Every comparison between two paths (the
/// main tree, a sidecar's recorded path, containment under a root) goes
/// through here: git reports `C:/x`, the Python tool's sidecars hold
/// `C:\x`, and Windows paths are case-insensitive. Returns what is left of
/// `child` below `parent`, or `None` when it is not under it.
#[must_use]
pub fn strip_under<'a>(child: &'a Path, parent: &Path) -> Option<&'a Path> {
    let mut rest = child.components();
    for want in parent.components() {
        let have = rest.next()?;
        if !same_component(want.as_os_str(), have.as_os_str()) {
            return None;
        }
    }
    Some(rest.as_path())
}

fn same_component(a: &std::ffi::OsStr, b: &std::ffi::OsStr) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

/// `a` and `b` name the same place, by `strip_under`'s rules.
#[must_use]
pub fn same_path(a: &Path, b: &Path) -> bool {
    strip_under(a, b).is_some_and(|rest| rest.as_os_str().is_empty())
}

/// `path` with the platform's own separators, the form the Python tool's
/// `str(Path.resolve())` printed and wrote into sidecars.
#[must_use]
pub fn native(path: &Path) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().replace('/', "\\"))
    } else {
        path.to_path_buf()
    }
}

/// Collapses `.`/`..` components by pure path arithmetic, with no
/// filesystem access.
pub fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn sub_table<'a>(t: &'a Table, key: &str) -> Option<&'a Table> {
    t.get(key).and_then(Value::as_table)
}

fn str_of(t: &Table, key: &str) -> Option<String> {
    t.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_of(t: &Table, key: &str) -> Option<bool> {
    t.get(key).and_then(Value::as_bool)
}

fn int_of(t: &Table, key: &str) -> Option<i64> {
    t.get(key).and_then(Value::as_integer)
}

fn list_of(t: &Table, key: &str) -> Option<Vec<String>> {
    t.get(key).and_then(Value::as_array).map(|arr| {
        arr.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

/// Copies every non-table entry of `from` into `into`; a nested table is a
/// namespace, not an option, and must never leak into the merged result.
fn merge_scalars(into: &mut Table, from: Option<&Table>) {
    let Some(from) = from else { return };
    for (k, v) in from {
        if !matches!(v, Value::Table(_)) {
            into.insert(k.clone(), v.clone());
        }
    }
}

// --------------------------------------------------------------- validation

#[derive(Clone, Copy)]
enum Kind {
    Str,
    Bool,
    List,
    Int,
}

impl Kind {
    const fn name(self) -> &'static str {
        match self {
            Self::Str => "str",
            Self::Bool => "bool",
            Self::List => "list",
            Self::Int => "int",
        }
    }

    fn matches(self, v: &Value) -> bool {
        match self {
            Self::Str => v.is_str(),
            Self::Bool => v.is_bool(),
            Self::List => v.is_array(),
            Self::Int => v.is_integer(),
        }
    }
}

const fn value_type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "str",
        Value::Integer(_) => "int",
        Value::Float(_) => "float",
        Value::Boolean(_) => "bool",
        Value::Datetime(_) => "datetime",
        Value::Array(_) => "list",
        Value::Table(_) => "dict",
    }
}

// track is deliberately absent (F24): an option this port no longer reads.
const OPTION_TYPES: &[(&str, Kind)] = &[
    ("root", Kind::Str),
    ("remote", Kind::Str),
    ("default_branch", Kind::Str),
    ("fetch", Kind::Bool),
    ("base", Kind::Str),
    ("branch", Kind::Str),
    ("dirname", Kind::Str),
    ("detach", Kind::Bool),
    ("copy", Kind::List),
    ("symlink", Kind::List),
    ("exec", Kind::List),
    ("exec_strict", Kind::Bool),
    ("exec_timeout", Kind::Int),
    ("shell", Kind::List),
    ("push", Kind::Bool),
    ("link_branch", Kind::Bool),
    ("set_state", Kind::Str),
    ("hook_type", Kind::Str),
];

const TEARDOWN_TYPES: &[(&str, Kind)] = &[
    ("fetch", Kind::Bool),
    ("require_clean", Kind::Bool),
    ("delete_branch", Kind::Str),
    ("purge_copied", Kind::Bool),
    ("prune", Kind::Bool),
    ("force", Kind::Bool),
    ("ttl_days", Kind::Int),
];

const NAMING_TYPES: &[(&str, Kind)] = &[
    ("llm", Kind::Bool),
    ("model", Kind::Str),
    ("timeout", Kind::Int),
    ("system", Kind::Str),
    ("stopwords", Kind::List),
];

const HERDR_TOP: &[(&str, Kind)] = &[("label_default", Kind::Str)];

const DELETE_BRANCH: &[&str] = &["never", "if_merged", "always"];
const MODES: &[&str] = &["new", "item", "pr", "branch", "hook"];
const HERDR_MODES: &[&str] = &["new", "item", "pr", "branch"];
const CONVENTIONAL_TYPES: &[&str] = &[
    "feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci",
];

fn suggest<'a>(key: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    candidates
        .map(|c| (c, strsim::normalized_levenshtein(key, c)))
        .filter(|(_, score)| *score >= 0.6)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(c, _)| c)
}

fn check_table(
    where_: &str,
    table: Option<&Table>,
    allowed: &[(&str, Kind)],
    problems: &mut Vec<String>,
) {
    check_table_skip(where_, table, allowed, &[], problems);
}

fn check_table_skip(
    where_: &str,
    table: Option<&Table>,
    allowed: &[(&str, Kind)],
    skip: &[&str],
    problems: &mut Vec<String>,
) {
    let Some(table) = table else { return };
    for (key, value) in table {
        if skip.contains(&key.as_str()) || matches!(value, Value::Table(_)) {
            continue;
        }
        match allowed.iter().find(|(k, _)| *k == key.as_str()) {
            None => {
                let hint = suggest(key, allowed.iter().map(|(k, _)| *k))
                    .map(|s| format!(" (did you mean {s}?)"))
                    .unwrap_or_default();
                problems.push(format!("{where_}: unknown key '{key}'{hint}"));
            }
            Some((_, kind)) if !kind.matches(value) => {
                problems.push(format!(
                    "{where_}: {key} should be {}, got {}",
                    kind.name(),
                    value_type_name(value)
                ));
            }
            Some((_, Kind::List)) => {
                // `list_of` would drop a non-string entry silently.
                let bad = value
                    .as_array()
                    .and_then(|a| a.iter().find(|v| !v.is_str()));
                if let Some(bad) = bad {
                    problems.push(format!(
                        "{where_}: {key} entries should be str, got {}",
                        value_type_name(bad)
                    ));
                }
            }
            Some(_) => {
                // ttl_days <= 0 is "unset" on purpose; a zero timeout is not.
                if let Some(n) = value.as_integer().filter(|n| *n <= 0)
                    && POSITIVE.contains(&key.as_str())
                {
                    problems.push(format!("{where_}: {key} should be > 0, got {n}"));
                }
            }
        }
    }
}

/// Int options that must be positive.
const POSITIVE: &[&str] = &["exec_timeout", "timeout"];

/// F6: `copy`/`symlink` entries must stay inside the tree, wherever
/// `copy`/`symlink` can be set (`[defaults]`, `[mode.x]`, `[mode.x.<provider>]`).
///
/// `Path::is_absolute()` alone is not enough: on Windows it is false for
/// `/etc/x`, `\etc\x` and `C:foo` (drive-relative), and
/// `Path::new(r"C:\repo\wt").join("/etc/secrets")` yields `C:/etc/secrets`,
/// still under the repo's drive. Refusing anything but `Normal`/`CurDir`
/// components catches a root, a drive prefix, or a `..` on every platform.
fn check_copy_symlink(where_: &str, table: &Table, problems: &mut Vec<String>) {
    for key in ["copy", "symlink"] {
        let Some(Value::Array(items)) = table.get(key) else {
            continue;
        };
        for item in items {
            let Some(entry) = item.as_str() else { continue };
            let escapes = Path::new(entry)
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
            if escapes {
                problems.push(format!(
                    "{where_}: {key} entry '{entry}' escapes the worktree"
                ));
            }
        }
    }
}

fn check_modes(cfg: &Table, where_: &str, problems: &mut Vec<String>) {
    let Some(mode_table) = sub_table(cfg, "mode") else {
        return;
    };
    for (mode, value) in mode_table {
        let Some(table) = value.as_table() else {
            continue;
        };
        if !MODES.contains(&mode.as_str()) {
            problems.push(format!(
                "{where_} [mode.{mode}]: unknown mode (one of {})",
                MODES.join(", ")
            ));
            continue;
        }
        let scope = format!("{where_} [mode.{mode}]");
        check_table(&scope, Some(table), OPTION_TYPES, problems);
        check_copy_symlink(&scope, table, problems);
        for (provider, sub) in table {
            let Some(provider_table) = sub.as_table() else {
                continue;
            };
            let pscope = format!("{where_} [mode.{mode}.{provider}]");
            check_table(&pscope, Some(provider_table), OPTION_TYPES, problems);
            check_copy_symlink(&pscope, provider_table, problems);
        }
    }
}

fn check_teardown(cfg: &Table, where_: &str, problems: &mut Vec<String>) {
    let Some(teardown) = sub_table(cfg, "teardown") else {
        return;
    };
    let top_scope = format!("{where_} [teardown]");
    check_table_skip(
        &top_scope,
        Some(teardown),
        TEARDOWN_TYPES,
        &["mode"],
        problems,
    );

    // F24: fetch only means anything at the top level; a mode override is
    // silently ignored at runtime, so validation refuses it there.
    let mode_types: Vec<(&str, Kind)> = TEARDOWN_TYPES
        .iter()
        .copied()
        .filter(|(k, _)| *k != "fetch")
        .collect();

    let mut scopes: Vec<(String, &Table)> = vec![(top_scope, teardown)];
    if let Some(mode_table) = sub_table(teardown, "mode") {
        for (mode, value) in mode_table {
            let Some(table) = value.as_table() else {
                continue;
            };
            let scope = format!("{where_} [teardown.mode.{mode}]");
            if !MODES.contains(&mode.as_str()) {
                problems.push(format!("{scope}: unknown mode"));
                continue;
            }
            check_table(&scope, Some(table), &mode_types, problems);
            scopes.push((scope, table));
        }
    }

    for (scope, table) in &scopes {
        let Some(Value::String(policy)) = table.get("delete_branch") else {
            continue;
        };
        if !DELETE_BRANCH.contains(&policy.as_str()) {
            problems.push(format!(
                "{scope}: delete_branch='{policy}' is not one of {} - a value wt does not \
                 recognise would delete the branch, not keep it",
                DELETE_BRANCH.join(", ")
            ));
        }
    }
}

fn check_naming(cfg: &Table, where_: &str, problems: &mut Vec<String>) {
    let Some(naming) = sub_table(cfg, "naming") else {
        return;
    };
    check_table(
        &format!("{where_} [naming]"),
        Some(naming),
        NAMING_TYPES,
        problems,
    );
    if let Some(Value::Table(tracker)) = naming.get("type_from_tracker") {
        for (k, v) in tracker {
            match v.as_str() {
                Some(typ) if CONVENTIONAL_TYPES.contains(&typ) => {}
                Some(typ) => problems.push(format!(
                    "{where_} [naming.type_from_tracker]: '{k}' maps to '{typ}', not one of {}",
                    CONVENTIONAL_TYPES.join(", ")
                )),
                // A non-string value (e.g. `bug = 1`) can never be one of
                // TYPES either; report it instead of skipping it silently.
                None => problems.push(format!(
                    "{where_} [naming.type_from_tracker]: '{k}' maps to {}, not one of {}",
                    value_type_name(v),
                    CONVENTIONAL_TYPES.join(", ")
                )),
            }
        }
    }
}

/// F24: `[herdr]` is validated. `label_default` must be a string; `[herdr.label]`
/// keys must be one of `new item pr branch`, each mapping to a string.
fn check_herdr(cfg: &Table, where_: &str, problems: &mut Vec<String>) {
    let Some(herdr) = sub_table(cfg, "herdr") else {
        return;
    };
    check_table(
        &format!("{where_} [herdr]"),
        Some(herdr),
        HERDR_TOP,
        problems,
    );
    let Some(label) = sub_table(herdr, "label") else {
        return;
    };
    for (mode, value) in label {
        if !HERDR_MODES.contains(&mode.as_str()) {
            problems.push(format!(
                "{where_} [herdr.label]: unknown mode '{mode}' (one of {})",
                HERDR_MODES.join(", ")
            ));
        } else if !value.is_str() {
            problems.push(format!(
                "{where_} [herdr.label]: {mode} should be str, got {}",
                value_type_name(value)
            ));
        }
    }
}

/// Refuses a config file wt would otherwise half-read. Reports every problem
/// at once: fixing typos one error per run is a miserable way to spend an
/// afternoon.
pub fn validate(cfg: &Table, where_: &str) -> Result<(), AppError> {
    let mut problems = Vec::new();
    let defaults = sub_table(cfg, "defaults");
    let defaults_scope = format!("{where_} [defaults]");
    check_table(&defaults_scope, defaults, OPTION_TYPES, &mut problems);
    if let Some(defaults) = defaults {
        check_copy_symlink(&defaults_scope, defaults, &mut problems);
    }
    check_modes(cfg, where_, &mut problems);
    check_teardown(cfg, where_, &mut problems);
    check_naming(cfg, where_, &mut problems);
    check_herdr(cfg, where_, &mut problems);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(AppError::ConfigInvalid(format!(
            "config problems:\n  {}",
            problems.join("\n  ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED_CONFIG: &str = include_str!("../../tests/fixtures/config.toml");

    fn parse(toml_src: &str) -> Table {
        toml::from_str(toml_src).unwrap()
    }

    fn shipped() -> Table {
        parse(SHIPPED_CONFIG)
    }

    // ------------------------------------------------------------ deep_merge

    #[test]
    fn test_tables_merge() {
        let got = deep_merge(&parse("[a]\nx = 1\ny = 2\n"), &parse("[a]\ny = 3\n"));
        assert_eq!(
            got.get("a").unwrap().get("x").unwrap().as_integer(),
            Some(1)
        );
        assert_eq!(
            got.get("a").unwrap().get("y").unwrap().as_integer(),
            Some(3)
        );
    }

    #[test]
    fn test_arrays_replace_not_append() {
        let got = deep_merge(
            &parse("copy = [\".env\", \".databricks\"]\n"),
            &parse("copy = [\".env.local\"]\n"),
        );
        assert_eq!(
            got.get("copy").unwrap().as_array().unwrap(),
            &vec![Value::String(".env.local".to_owned())]
        );
    }

    // ---------------------------------------------------- TestConfigPrecedence

    fn precedence_cfg() -> Table {
        parse(
            r#"
            [defaults]
            base = "origin/{default_branch}"
            copy = [".env"]
            fetch = true

            [mode.item]
            copy = [".env", ".databricks"]
            link_branch = true

            [mode.item.github]
            link_branch = false
            "#,
        )
    }

    #[test]
    fn test_defaults_layer() {
        assert_eq!(resolve(&precedence_cfg(), "new", "ado").copy, vec![".env"]);
    }

    #[test]
    fn test_mode_overrides_defaults() {
        let conf = resolve(&precedence_cfg(), "item", "ado");
        assert_eq!(conf.copy, vec![".env", ".databricks"]);
        assert_eq!(conf.link_branch, Some(true));
    }

    #[test]
    fn test_provider_overrides_mode() {
        assert_eq!(
            resolve(&precedence_cfg(), "item", "github").link_branch,
            Some(false)
        );
    }

    #[test]
    fn test_provider_subtables_never_leak_as_options() {
        // Requesting provider "ado" must not pick up [mode.item.github]'s
        // link_branch = false; the resolved value stays [mode.item]'s own.
        assert_eq!(
            resolve(&precedence_cfg(), "item", "ado").link_branch,
            Some(true)
        );
    }

    #[test]
    fn test_teardown_namespace_is_separate() {
        let cfg = parse(
            r#"
            [teardown]
            require_clean = true
            delete_branch = "if_merged"

            [teardown.mode.pr]
            require_clean = false

            [teardown.mode.branch]
            delete_branch = "never"
            "#,
        );
        assert!(resolve_teardown(&cfg, "new").require_clean);
        assert!(!resolve_teardown(&cfg, "pr").require_clean);
        assert_eq!(resolve_teardown(&cfg, "branch").delete_branch, "never");
    }

    #[test]
    fn test_shipped_config_protects_review_trees() {
        let cfg = shipped();
        assert_eq!(
            resolve(&cfg, "pr", "ado").copy,
            Vec::<String>::new(),
            "no secrets in PR trees"
        );
        assert_eq!(
            resolve(&cfg, "branch", "ado").copy,
            Vec::<String>::new(),
            "no secrets in branch trees"
        );
        assert_eq!(resolve_teardown(&cfg, "branch").delete_branch, "never");
        assert_eq!(
            resolve_teardown(&cfg, "hook").force,
            Some(false),
            "the hook must not force"
        );
    }

    // ------------------------------------------------- TestNamespacesAreNotOptions

    #[test]
    fn test_an_unknown_sub_table_does_not_leak_into_options() {
        let cfg = parse(
            r"
            [defaults]
            fetch = true

            [mode.item]
            push = true

            [mode.item.gitlab]
            push = false
            ",
        );
        assert!(resolve(&cfg, "item", "ado").push);
    }

    #[test]
    fn test_the_provider_sub_table_still_applies() {
        let cfg = parse(
            r"
            [mode.item]
            push = true

            [mode.item.github]
            push = false
            ",
        );
        assert!(!resolve(&cfg, "item", "github").push);
        assert!(resolve(&cfg, "item", "ado").push);
    }

    // ------------------------------------------------------- TestShippedDefaults

    #[test]
    fn test_review_modes_never_inherit_copy() {
        let cfg = deep_merge(
            &defaults(),
            &parse("[defaults]\ncopy = [\".env\", \".databricks\"]\n"),
        );
        for mode in ["pr", "branch"] {
            assert_eq!(
                resolve(&cfg, mode, "ado").copy,
                Vec::<String>::new(),
                "{mode}"
            );
        }
        assert_eq!(
            resolve(&cfg, "new", "ado").copy,
            vec![".env", ".databricks"]
        );
    }

    // ------------------------------------------------------------- F23 (hook)

    #[test]
    fn test_hook_mode_inherits_from_mode_new() {
        let cfg = parse(
            r#"
            [mode.new]
            hook_type = "chore"

            [mode.hook]
            fetch = false
            "#,
        );
        let conf = resolve(&cfg, "hook", "none");
        assert_eq!(conf.hook_type.as_deref(), Some("chore"));
        assert!(!conf.fetch);
    }

    #[test]
    fn test_hook_mode_layers_new_its_provider_then_hook_and_its_provider() {
        // Python's hook-create actually resolves mode "new", so
        // [mode.new.<provider>] must still apply; [mode.hook] and
        // [mode.hook.<provider>] then layer on top of that (F23).
        let cfg = parse(
            r#"
            [mode.new]
            hook_type = "chore"
            push = true

            [mode.new.github]
            push = false

            [mode.hook]
            hook_type = "fix"
            fetch = false

            [mode.hook.github]
            fetch = true
            "#,
        );
        let conf = resolve(&cfg, "hook", "github");
        assert_eq!(
            conf.hook_type.as_deref(),
            Some("fix"),
            "mode.hook wins over mode.new"
        );
        assert!(!conf.push, "mode.new.<provider> still applies");
        assert!(conf.fetch, "mode.hook.<provider> applies last");
    }

    // ---------------------------------------------------------- TestConfigValidation

    fn problems(cfg: &Table) -> String {
        match validate(cfg, "config.toml") {
            Err(AppError::ConfigInvalid(msg)) => msg,
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    #[test]
    fn test_a_misspelt_delete_branch_is_refused() {
        let msg = problems(&parse("[teardown]\ndelete_branch = \"nver\"\n"));
        assert!(msg.contains("delete_branch"));
        assert!(msg.contains("would delete the branch"));
    }

    #[test]
    fn test_a_misspelt_delete_branch_under_a_mode_is_refused() {
        let msg = problems(&parse("[teardown.mode.pr]\ndelete_branch = \"keep\"\n"));
        assert!(msg.contains("[teardown.mode.pr]"));
    }

    #[test]
    fn test_an_unknown_key_suggests_the_real_one() {
        let msg = problems(&parse("[teardown]\nrequre_clean = true\n"));
        assert!(msg.contains("did you mean require_clean"));
    }

    #[test]
    fn test_a_quoted_number_is_refused() {
        let msg = problems(&parse("[naming]\ntimeout = \"60\"\n"));
        assert!(msg.contains("should be int"));
    }

    #[test]
    fn test_a_bool_is_not_an_int() {
        let msg = problems(&parse("[defaults]\nexec_timeout = true\n"));
        assert!(msg.contains("should be int"));
    }

    #[test]
    fn a_timeout_of_zero_or_less_is_refused() {
        let msg = problems(&parse("[defaults]\nexec_timeout = 0\n"));
        assert!(msg.contains("exec_timeout should be > 0, got 0"), "{msg}");
        let msg = problems(&parse("[mode.new]\nexec_timeout = -5\n"));
        assert!(msg.contains("exec_timeout should be > 0, got -5"), "{msg}");
        let msg = problems(&parse("[naming]\ntimeout = 0\n"));
        assert!(msg.contains("timeout should be > 0, got 0"), "{msg}");
        // ttl_days <= 0 stays "unset", not a mistake.
        validate(&parse("[teardown]\nttl_days = 0\n"), "config.toml").unwrap();
    }

    #[test]
    fn a_non_string_list_entry_is_refused() {
        for (text, key) in [
            ("[defaults]\ncopy = [\".env\", 1]\n", "copy"),
            ("[defaults]\nsymlink = [true]\n", "symlink"),
            ("[mode.new]\nexec = [[\"x\"]]\n", "exec"),
            ("[defaults]\nshell = [\"bash\", 2]\n", "shell"),
            ("[naming]\nstopwords = [\"a\", 1.5]\n", "stopwords"),
        ] {
            let msg = problems(&parse(text));
            assert!(
                msg.contains(&format!("{key} entries should be str")),
                "{msg}"
            );
        }
    }

    #[test]
    fn test_an_unknown_mode_is_refused() {
        let msg = problems(&parse("[mode.itm]\npush = true\n"));
        assert!(msg.contains("unknown mode"));
    }

    #[test]
    fn test_a_bad_conventional_type_is_refused() {
        let msg = problems(&parse("[naming.type_from_tracker]\nbug = \"fixx\"\n"));
        assert!(msg.contains("type_from_tracker"));
    }

    #[test]
    fn test_a_non_string_conventional_type_is_refused() {
        // Never a member of TYPES either; must be reported, not skipped.
        let msg = problems(&parse("[naming.type_from_tracker]\nbug = 1\n"));
        assert!(msg.contains("type_from_tracker"));
        assert!(msg.contains("int"));
    }

    #[test]
    fn test_every_problem_is_reported_at_once() {
        let msg = problems(&parse(
            "[defaults]\nferch = true\n[naming]\ntimeout = \"60\"\n",
        ));
        assert!(msg.contains("ferch"));
        assert!(msg.contains("timeout"));
    }

    #[test]
    fn test_the_shipped_config_validates() {
        validate(&shipped(), "config.toml").unwrap();
    }

    #[test]
    fn test_a_provider_sub_table_is_checked_too() {
        let msg = problems(&parse("[mode.item.github]\nlnk_branch = false\n"));
        assert!(msg.contains("[mode.item.github]"));
    }

    // --------------------------------------------------------------- F6 (copy/symlink)

    #[test]
    fn test_absolute_and_parent_traversal_entries_are_refused_on_every_os() {
        // Refused on both Windows and Unix: a rooted path (however it is
        // spelt) and a `..` component.
        for entry in ["/etc/secrets", "../x"] {
            let msg = problems(&parse(&format!("[defaults]\ncopy = [{entry:?}]\n")));
            assert!(msg.contains("escapes the worktree"), "{entry}: {msg}");
        }
    }

    #[test]
    fn test_a_parent_traversal_symlink_entry_is_refused() {
        let msg = problems(&parse("[mode.new]\nsymlink = [\"../../../canary\"]\n"));
        assert!(msg.contains("escapes the worktree"));
    }

    #[test]
    fn test_windows_drive_and_backslash_entries_are_refused() {
        // `Path::is_absolute()` alone misses these on Windows (drive-relative
        // `C:foo`, a rooted-but-driveless `\etc\x`); `Component` catches both.
        // On Unix these strings are plain relative filenames (no drive or
        // backslash-separator concept), so they are not escapes there.
        if !cfg!(windows) {
            return;
        }
        for entry in [r"\etc\x", "C:foo", "C:/x"] {
            let msg = problems(&parse(&format!("[defaults]\ncopy = [{entry:?}]\n")));
            assert!(msg.contains("escapes the worktree"), "{entry}: {msg}");
        }
    }

    // -------------------------------------------------------------------- F24

    #[test]
    fn test_track_is_no_longer_a_recognised_option() {
        let msg = problems(&parse("[defaults]\ntrack = true\n"));
        assert!(msg.contains("unknown key 'track'"));
    }

    #[test]
    fn test_teardown_mode_fetch_is_refused() {
        let msg = problems(&parse("[teardown.mode.pr]\nfetch = true\n"));
        assert!(msg.contains("[teardown.mode.pr]"));
        assert!(msg.contains("unknown key 'fetch'"));
    }

    #[test]
    fn test_herdr_label_default_type_is_checked() {
        let msg = problems(&parse("[herdr]\nlabel_default = 1\n"));
        assert!(msg.contains("should be str"));
    }

    #[test]
    fn test_herdr_unknown_label_mode_is_refused() {
        let msg = problems(&parse("[herdr.label]\ngitlab = \"x\"\n"));
        assert!(msg.contains("[herdr.label]"));
        assert!(msg.contains("unknown mode"));
    }

    #[test]
    fn test_shipped_herdr_labels_validate() {
        // Covered by test_the_shipped_config_validates too; asserted directly
        // so a regression in check_herdr fails here, not just generically.
        let cfg = shipped();
        let herdr = cfg.get("herdr").unwrap().as_table().unwrap();
        assert_eq!(
            herdr.get("label_default").unwrap().as_str(),
            Some("{repo} · {branch}")
        );
    }

    // ------------------------------------------------------------------------ load

    #[test]
    fn test_load_merges_global_then_repo_missing_files_are_fine() {
        // No second file at all, same as a repo with no .wt.toml.
        let cfg = load(&[("config.toml", "[defaults]\ncopy = [\".env\"]\n")]).unwrap();
        assert_eq!(resolve(&cfg, "new", "none").copy, vec![".env"]);
    }

    #[test]
    fn test_load_repo_file_overrides_global() {
        let cfg = load(&[
            ("config.toml", "[defaults]\ncopy = [\".env\"]\n"),
            (".wt.toml", "[defaults]\ncopy = [\".env.local\"]\n"),
        ])
        .unwrap();
        assert_eq!(resolve(&cfg, "new", "none").copy, vec![".env.local"]);
    }

    // -------------------------------------------------------------- naming_of

    #[test]
    fn test_naming_of_reads_the_table() {
        let cfg = parse(
            "[naming]\nllm = true\nmodel = \"sonnet\"\ntimeout = 30\nsystem = \"x\"\nstopwords = [\"foo\"]\n",
        );
        let n = naming_of(&cfg);
        assert!(n.llm);
        assert_eq!(n.model, "sonnet");
        assert_eq!(n.timeout, 30);
        assert_eq!(n.system.as_deref(), Some("x"));
        assert_eq!(n.stopwords, vec!["foo".to_owned()]);
    }

    #[test]
    fn test_naming_of_defaults_when_absent() {
        let n = naming_of(&Table::new());
        assert!(!n.llm);
        assert_eq!(n.model, "haiku");
        assert_eq!(n.timeout, 60);
        assert_eq!(n.system, None);
        assert!(n.stopwords.is_empty());
    }

    // ------------------------------------------------------------- remote_of

    #[test]
    fn test_remote_of_defaults_to_origin() {
        assert_eq!(remote_of(&Table::new()), "origin");
    }

    #[test]
    fn test_remote_of_reads_the_configured_remote() {
        let cfg = parse("[defaults]\nremote = \"upstream\"\n");
        assert_eq!(remote_of(&cfg), "upstream");
    }

    // ----------------------------------------------------------- wt_root_of

    #[test]
    fn test_wt_root_of_fills_repo_and_collapses_parent_dirs() {
        let root = Path::new("/home/bk/repo");
        let opts = Options::from_table(&Table::new());
        assert_eq!(
            wt_root_of(root, &opts),
            Path::new("/home/bk/repo.worktrees")
        );
    }

    #[test]
    fn test_wt_root_of_honours_a_mode_scoped_root() {
        let root = Path::new("/home/bk/repo");
        let cfg = parse("[mode.pr]\nroot = \"../{repo}.reviews\"\n");
        let opts = resolve(&cfg, "pr", "none");
        assert_eq!(wt_root_of(root, &opts), Path::new("/home/bk/repo.reviews"));
    }

    // ------------------------------------------------------------ path identity

    #[test]
    fn strip_under_gives_the_rest_and_refuses_siblings() {
        let root = Path::new("/r/repo.worktrees");
        assert_eq!(
            strip_under(Path::new("/r/repo.worktrees/feat/x"), root),
            Some(Path::new("feat/x"))
        );
        assert_eq!(strip_under(Path::new("/r/repo.worktrees2/x"), root), None);
        assert_eq!(strip_under(Path::new("/r"), root), None);
        assert!(same_path(root, Path::new("/r/repo.worktrees/")));
        assert!(!same_path(Path::new("/r/repo.worktrees/x"), root));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_match_across_separators_and_case() {
        let git_form = Path::new("C:/Users/bk/Repo.worktrees/feat/x");
        let python_form = Path::new(r"c:\users\BK\repo.worktrees\feat\x");
        assert!(same_path(git_form, python_form));
        assert_eq!(
            native(git_form),
            PathBuf::from(r"C:\Users\bk\Repo.worktrees\feat\x")
        );
    }

    #[test]
    fn test_load_names_the_invalid_file() {
        let err = load(&[
            ("config.toml", "[defaults]\ncopy = [\".env\"]\n"),
            ("repo/.wt.toml", "[defaults]\nferch = true\n"),
        ])
        .unwrap_err();
        let AppError::ConfigInvalid(msg) = err else {
            panic!("expected ConfigInvalid, got {err:?}");
        };
        assert!(msg.contains("ferch"));
        assert!(msg.contains("repo/.wt.toml"), "{msg}");
    }
}
