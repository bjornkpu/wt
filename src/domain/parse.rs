use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use crate::error::AppError;

/// One record from `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    /// git's own `prunable` line: the registration is still there, but
    /// something about its directory is broken - either it was deleted by
    /// hand, or only its own `.git` link was (leaving the rest of the tree
    /// on disk). The branch and sidecar can still be found by path/name, so
    /// `rm` must still be able to reach it even though `ls`/`cd`/`complete`
    /// have nothing useful to do with a directory that is not there.
    pub prunable: bool,
    /// Whether the tree's directory has actually vanished, as opposed to
    /// merely having a broken `.git` link (see `prunable`). Parsing the
    /// porcelain text alone can never know this - it is always `false`
    /// straight out of `parse_porcelain` - so `io::run::gather` fills it in
    /// with a filesystem check once, right after parsing, for every
    /// worktree every other consumer (`ls`, `cd`, `rm`, the sweeps) then
    /// reads instead of probing again.
    pub gone: bool,
}

/// Parses `git worktree list --porcelain`'s block format. A bare repo carries
/// no HEAD line and is skipped; a prunable record (its directory deleted by
/// hand) keeps its HEAD/branch and is kept too, flagged `prunable` rather
/// than dropped, so `wt rm` can still reach it. Every other record is kept,
/// including one with no trailing blank line.
#[must_use]
pub fn parse_porcelain(text: &str) -> Vec<Worktree> {
    text.replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(parse_block)
        .collect()
}

fn parse_block(block: &str) -> Option<Worktree> {
    let mut worktree: Option<&str> = None;
    let mut branch: Option<&str> = None;
    let mut head = String::new();
    let mut bare = false;
    let mut prunable = false;
    for line in block.lines() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "worktree" => worktree = Some(value),
            "branch" => branch = Some(value.strip_prefix("refs/heads/").unwrap_or(value)),
            "HEAD" => value.clone_into(&mut head),
            "bare" => bare = true,
            "prunable" => prunable = true,
            _ => {}
        }
    }
    let worktree = worktree?;
    if bare {
        return None;
    }
    Some(Worktree {
        path: PathBuf::from(worktree),
        branch: branch.filter(|b| !b.is_empty()).map(str::to_owned),
        head,
        prunable,
        gone: false,
    })
}

/// Main worktree root, given the (already absolute, resolved) common `.git`
/// directory: its parent when the common dir is itself named `.git`,
/// otherwise the common dir is the root (a bare repo, or a linked worktree
/// whose common dir already points at the shared store).
#[must_use]
pub fn main_root_of(common_dir: &std::path::Path) -> PathBuf {
    if common_dir.file_name() == Some(std::ffi::OsStr::new(".git")) {
        common_dir
            .parent()
            .map_or_else(|| common_dir.to_path_buf(), std::path::Path::to_path_buf)
    } else {
        common_dir.to_path_buf()
    }
}

/// Where wt's sidecars live: beside the shared `.git`, so they are in no
/// working tree.
#[must_use]
pub fn meta_dir_of(common_dir: &std::path::Path) -> PathBuf {
    common_dir.join("wt")
}

/// F5: strips a known `refs/.../` prefix, keeping any further slashes. A
/// naive "everything up to the last /" would truncate a default branch like
/// "release/main" down to "main".
fn strip_ref_prefix(refname: &str, prefix: &str) -> String {
    refname.strip_prefix(prefix).unwrap_or(refname).to_owned()
}

/// The default branch from `symbolic-ref --quiet refs/remotes/<remote>/HEAD`'s
/// stdout, or `None` when that ref is unset (a `--single-branch` clone).
#[must_use]
pub fn default_branch_from_symbolic_ref(remote: &str, output: &str) -> Option<String> {
    let output = output.trim();
    if output.is_empty() {
        return None;
    }
    Some(strip_ref_prefix(output, &format!("refs/remotes/{remote}/")))
}

/// The default branch from `ls-remote --symref <remote> HEAD`'s stdout: the
/// `ref: refs/heads/<branch>` line.
#[must_use]
pub fn default_branch_from_ls_remote(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("ref: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(|refname| strip_ref_prefix(refname, "refs/heads/"))
}

/// The `for-each-ref` format `gone_upstreams` parses.
pub const GONE_FORMAT: &str = "--format=%(refname:short) %(upstream:short) %(upstream:track)";

/// Branches whose upstream git reports `[gone]`, by branch name, out of
/// `for-each-ref GONE_FORMAT refs/heads`. A squash merge never makes the
/// branch an ancestor of the default branch; its remote branch vanishing
/// when the PR completed is what is left to see.
#[must_use]
pub fn gone_upstreams(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (branch, rest) = line.split_once(' ')?;
            let (upstream, track) = rest.split_once(' ')?;
            (track.trim() == "[gone]").then(|| (branch.to_owned(), upstream.to_owned()))
        })
        .collect()
}

/// What a branch's own reflog says about whether real work has ever landed
/// on it, from `git reflog show --format=%gs refs/heads/<b>`'s stdout. F1:
/// a fresh branch only "matches" `--merged` because it is identical to the
/// default branch, not because anything landed on it - and merely having
/// more than one reflog entry is not evidence either way, since a
/// fast-forward pull, a fast-forward merge, a rename, a reset or a no-op
/// rebase each add an entry without any commit of the branch's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchHistory {
    /// No entry represents real work: only bookkeeping (creation, a
    /// fast-forward pull/merge, a rename, a reset, a no-op rebase).
    CreationOnly,
    /// At least one entry is a real commit, a cherry-pick, a revert, or an
    /// `am`-applied patch: something has actually happened here.
    HasCommits,
    /// The reflog could not be read (missing, or the git call failed).
    /// Never treated as fresh: a destructive sweep must not reap what it
    /// cannot positively confirm has commits, so this fails closed the
    /// same way `CreationOnly` does, without claiming to be it.
    Unknown,
}

/// Reflog subject prefixes that mean a real commit landed on this branch,
/// confirmed against real git output: `commit: ...`, `commit (initial):
/// ...`, `commit (amend): ...` and `commit (merge): ...` (a manual commit
/// after resolving a conflict) all start with `commit`; `cherry-pick: ...`,
/// `revert: ...` and `am: ...` each have their own prefix.
///
/// Everything else moves the ref without the branch accumulating anything
/// of its own: `branch: Created from ...`, `pull ...: Fast-forward`,
/// `merge ...: Fast-forward`, `Branch: renamed ...`, `reset: moving to
/// ...`, a no-op `rebase (start|finish): ...`.
const REAL_WORK_PREFIXES: [&str; 4] = ["commit", "cherry-pick", "revert", "am"];

/// Classifies `branch_history`'s reflog text: `HasCommits` when any line's
/// subject starts with one of `REAL_WORK_PREFIXES`, `Unknown` when there is
/// no reflog at all (not positive evidence either way), else
/// `CreationOnly` - which, despite the name, also covers a fresh branch
/// that has since been fast-forwarded, renamed, reset or rebased with
/// nothing to replay: none of that is a commit of its own.
#[must_use]
pub fn branch_history(reflog: &str) -> BranchHistory {
    if reflog.trim().is_empty() {
        return BranchHistory::Unknown;
    }
    let has_commits = reflog
        .lines()
        .any(|line| REAL_WORK_PREFIXES.iter().any(|p| line.starts_with(p)));
    if has_commits {
        BranchHistory::HasCommits
    } else {
        BranchHistory::CreationOnly
    }
}

/// The provider a remote URL implies, by host only: which
/// `[mode.x.<provider>]` sub-table to look under (F19). `provider::from_url`
/// builds the full provider (org/project/repo) on top of this.
#[must_use]
pub fn provider_of_url(url: &str) -> &'static str {
    let host = split_remote(url).map_or_else(String::new, |(h, _)| h.to_lowercase());
    if host == "dev.azure.com" || host == "ssh.dev.azure.com" || host.ends_with(".visualstudio.com")
    {
        return "ado";
    }
    if host == "github.com" {
        return "github";
    }
    "none"
}

/// The host (original case, no user or port) and the path (no leading `/`)
/// of an `https://`/`ssh://` URL or a scp-style `git@host:path` remote.
#[must_use]
pub fn split_remote(url: &str) -> Option<(&str, &str)> {
    let (authority, path) = if let Some((_, rest)) = url.split_once("://") {
        rest.split_once('/').unwrap_or((rest, ""))
    } else {
        let (_, rest) = url.split_once('@')?;
        let (host, path) = rest.split_once(':').unwrap_or((rest, ""));
        (host.split('/').next().unwrap_or(""), path)
    };
    let host_and_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    Some((host_and_port.split(':').next().unwrap_or(""), path))
}

/// A worktree's JSON sidecar, with the Python tool's own keys. Read leniently
/// (`parse_meta`), written in the Python tool's key order (`sidecar_json`):
/// the fields as declared, `branch` as `null` when detached, the optional
/// extras only when set.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Meta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detached: Option<bool>,
    pub copied: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exec_failed: Vec<String>,
}

/// The sidecar's text, byte for byte what the Python tool's
/// `write_text(json.dumps(meta, indent=2))` writes: `ensure_ascii` escapes
/// (`å` becomes `\u00e5`, surrogate pairs above the BMP), and CRLF on
/// Windows, where `write_text` translates newlines.
pub fn sidecar_json(meta: &Meta) -> Result<String, serde_json::Error> {
    let ascii = ensure_ascii(&serde_json::to_string_pretty(meta)?);
    Ok(if cfg!(windows) {
        ascii.replace('\n', "\r\n")
    } else {
        ascii
    })
}

/// JSON text with every non-ASCII character as a `\uXXXX` escape, as
/// Python's `json.dumps` writes it. Only valid on JSON text: outside a
/// string there is nothing non-ASCII to escape.
#[must_use]
pub fn ensure_ascii(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_ascii() && c != '\u{7f}' {
            out.push(c);
        } else {
            for unit in c.encode_utf16(&mut [0; 2]) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

/// The sidecar's file name for a leaf: Python's
/// `urllib.parse.quote(leaf, safe="") + ".json"`, so `feat/x` is
/// `feat%2Fx.json`.
#[must_use]
pub fn sidecar_file_name(leaf: &str) -> String {
    quote(leaf) + ".json"
}

/// Python's `urllib.parse.quote(s, safe="")`.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// `created`'s format: Python's `datetime.now(UTC).isoformat()`, which
/// drops the fraction when it is exactly zero.
#[must_use]
pub fn iso_utc(ts: jiff::Timestamp) -> String {
    let micros = ts.subsec_microsecond();
    let fraction = if micros > 0 {
        format!(".{micros:06}")
    } else {
        String::new()
    };
    format!("{}{fraction}+00:00", ts.strftime("%Y-%m-%dT%H:%M:%S"))
}

/// The result of decoding one sidecar file's text.
pub enum SidecarRead {
    /// A JSON object, decoded field by field.
    Meta(Box<Meta>),
    /// F9: valid JSON, but not an object (an array, a string, a number...).
    /// The caller warns and skips it, rather than crashing on `.get()`.
    NotObject,
    /// Not valid JSON at all.
    Invalid,
}

fn str_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    obj.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn bool_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<bool> {
    obj.get(key).and_then(serde_json::Value::as_bool)
}

fn list_field(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `created` specifically must stay distinguishable from "no `created` key
/// at all" even when its value is not a string: `age_str` (F9) treats
/// "present but unparseable" ("?") differently from "absent" (""). A
/// mistyped `created` (say, a bare JSON number) has no string form of its
/// own, so its JSON text stands in - `age_str`'s own timestamp parse then
/// fails on it exactly the same way a garbled string would.
fn created_field(obj: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let value = obj.get("created")?;
    Some(
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned),
    )
}

/// Decodes one sidecar file's text field by field, so one mistyped field
/// (F9: e.g. `"created": 123`, `"copied": null`, `"exec_failed": true`)
/// only drops that field, never the whole sidecar - unlike a derived
/// `Deserialize`, which fails the entire object. A field that is simply
/// absent, or present with the wrong shape, is read as its type's empty
/// value; only `created` (see `created_field`) needs to keep the
/// distinction between absent and malformed. Pure: the caller does the
/// reading and the warning.
#[must_use]
pub fn parse_meta(text: &str) -> SidecarRead {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return SidecarRead::Invalid;
    };
    let Some(obj) = value.as_object() else {
        return SidecarRead::NotObject;
    };
    SidecarRead::Meta(Box::new(Meta {
        mode: str_field(obj, "mode"),
        path: str_field(obj, "path"),
        branch: str_field(obj, "branch"),
        base: str_field(obj, "base"),
        detached: bool_field(obj, "detached"),
        copied: list_field(obj, "copied"),
        created: created_field(obj),
        id: str_field(obj, "id"),
        title: str_field(obj, "title"),
        pr_branch: str_field(obj, "pr_branch"),
        via: str_field(obj, "via"),
        exec_failed: list_field(obj, "exec_failed"),
    }))
}

/// The JSON type name Python's `type(payload).__name__` would print for a
/// hook payload that parsed but was not an object.
fn json_type_name(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "NoneType",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        serde_json::Value::Number(_) => "float",
        serde_json::Value::String(_) => "str",
        serde_json::Value::Array(_) => "list",
        serde_json::Value::Object(_) => "dict",
    }
}

/// The JSON Claude Code writes to a hook's stdin. Decoded leniently
/// (invalid UTF-8 becomes the replacement character, as Python's
/// `.decode("utf-8", "replace")` does); empty input is `{}`. Invalid JSON or
/// a JSON value that is not an object is refused rather than crashing
/// later on a `.get()` that assumes one.
pub fn parse_hook_payload(
    bytes: &[u8],
) -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    let raw = String::from_utf8_lossy(bytes);
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|_source| AppError::HookPayloadNotJson)?;
    match value {
        serde_json::Value::Object(obj) => Ok(obj),
        other => Err(AppError::HookPayloadNotObject {
            kind: json_type_name(&other).to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------- gone_upstreams

    #[test]
    fn gone_upstreams_keeps_only_the_gone_ones() {
        let out = "feat/a origin/feat/a [gone]\nfeat/b origin/feat/b [ahead 1]\nfeat/c  \nmain origin/main \n";
        let gone = gone_upstreams(out);
        assert_eq!(gone.len(), 1, "{gone:?}");
        assert_eq!(
            gone.get("feat/a").map(String::as_str),
            Some("origin/feat/a")
        );
    }

    // ------------------------------------------------------------ F1: branch_history

    #[test]
    fn a_single_creation_entry_is_creation_only() {
        assert_eq!(
            branch_history("branch: Created from main"),
            BranchHistory::CreationOnly
        );
    }

    #[test]
    fn a_commit_on_top_of_the_creation_entry_has_commits() {
        assert_eq!(
            branch_history("commit: work\nbranch: Created from main"),
            BranchHistory::HasCommits
        );
    }

    #[test]
    fn a_single_commit_line_has_commits() {
        assert_eq!(branch_history("commit: work"), BranchHistory::HasCommits);
    }

    #[test]
    fn an_empty_reflog_is_unknown_not_fresh() {
        assert_eq!(branch_history(""), BranchHistory::Unknown);
    }

    // The lines below are real `git reflog show --format=%gs` output,
    // captured against a real repository for each operation (see the
    // fix-round-1 report for the exact commands).

    #[test]
    fn a_fast_forward_pull_does_not_count_as_a_commit() {
        assert_eq!(
            branch_history(
                "pull -q --ff-only origin main: Fast-forward\nbranch: Created from main"
            ),
            BranchHistory::CreationOnly,
            "a fresh branch that was only ever fast-forwarded has nothing of its own"
        );
    }

    #[test]
    fn a_fast_forward_merge_does_not_count_as_a_commit() {
        assert_eq!(
            branch_history("merge origin/main: Fast-forward\nbranch: Created from feat/x"),
            BranchHistory::CreationOnly
        );
    }

    #[test]
    fn a_branch_rename_does_not_count_as_a_commit() {
        assert_eq!(
            branch_history(
                "Branch: renamed refs/heads/feat/a to refs/heads/feat/b\nbranch: Created from main"
            ),
            BranchHistory::CreationOnly
        );
    }

    #[test]
    fn a_reset_does_not_count_as_a_commit() {
        assert_eq!(
            branch_history("reset: moving to HEAD~1\nbranch: Created from main"),
            BranchHistory::CreationOnly
        );
    }

    #[test]
    fn a_no_op_rebase_does_not_count_as_a_commit() {
        // `git rebase main` with nothing to replay does not even add an
        // entry: only the creation line is left.
        assert_eq!(
            branch_history("branch: Created from main"),
            BranchHistory::CreationOnly
        );
    }

    #[test]
    fn a_real_rebase_still_finds_the_original_commit_entry() {
        // The rebase itself adds "rebase (finish): ..." on top, but the
        // original "commit: work" entry survives further down.
        assert_eq!(
            branch_history(
                "rebase (finish): refs/heads/feat/x onto abc123\ncommit: work\nbranch: Created from main"
            ),
            BranchHistory::HasCommits
        );
    }

    #[test]
    fn a_cherry_pick_counts_as_a_commit() {
        assert_eq!(
            branch_history("cherry-pick: third\nbranch: Created from main"),
            BranchHistory::HasCommits
        );
    }

    #[test]
    fn a_revert_counts_as_a_commit() {
        assert_eq!(
            branch_history("revert: Revert \"second\"\nbranch: Created from main"),
            BranchHistory::HasCommits
        );
    }

    #[test]
    fn an_am_applied_patch_counts_as_a_commit() {
        assert_eq!(
            branch_history("am: second\nbranch: Created from main"),
            BranchHistory::HasCommits
        );
    }

    #[test]
    fn an_amended_or_initial_commit_counts_too() {
        assert_eq!(
            branch_history("commit (amend): second (amended)"),
            BranchHistory::HasCommits
        );
        assert_eq!(
            branch_history("commit (initial): init"),
            BranchHistory::HasCommits
        );
    }

    // ------------------------------------------------------- TestParsePorcelain

    const SAMPLE: &str = "worktree /repo
HEAD abc123def4567890
branch refs/heads/main

worktree /repo.worktrees/feat/x
HEAD 0123456789abcdef
detached

worktree /bare-one
bare

worktree /stale
HEAD deadbeef00000000
branch refs/heads/feat/deleted-by-hand
prunable gitdir file points to non-existent location

worktree /repo.worktrees/feat/last
HEAD ffff111122223333
branch refs/heads/feat/last
";

    fn by_name(text: &str) -> std::collections::HashMap<String, Worktree> {
        parse_porcelain(text)
            .into_iter()
            .map(|w| {
                let name = w
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_owned();
                (name, w)
            })
            .collect()
    }

    #[test]
    fn test_skips_bare_but_keeps_prunable() {
        let rows = by_name(SAMPLE);
        assert!(!rows.contains_key("bare-one"));
        let stale = rows.get("stale").expect("prunable record must be kept");
        assert!(stale.prunable);
        assert_eq!(stale.branch.as_deref(), Some("feat/deleted-by-hand"));
        assert_eq!(stale.head, "deadbeef00000000");
    }

    #[test]
    fn test_a_normal_record_is_not_prunable() {
        let rows = by_name(SAMPLE);
        assert!(!rows.get("repo").expect("must be present").prunable);
    }

    #[test]
    fn test_every_record_has_head_and_branch() {
        for w in parse_porcelain(SAMPLE) {
            assert!(w.head.chars().count() >= 8);
        }
    }

    #[test]
    fn test_detached_has_no_branch() {
        let rows = by_name(SAMPLE);
        assert_eq!(rows.get("x").and_then(|w| w.branch.as_deref()), None);
        assert_eq!(
            rows.get("repo").and_then(|w| w.branch.as_deref()),
            Some("main")
        );
    }

    #[test]
    fn test_trailing_record_without_blank_line() {
        assert!(
            by_name(SAMPLE).contains_key("last"),
            "final record must be flushed"
        );
    }

    // ------------------------------------------------------------------ main_root/meta_dir

    #[test]
    fn main_root_is_the_parent_of_a_dot_git_common_dir() {
        let common = PathBuf::from("/repo/.git");
        assert_eq!(main_root_of(&common), PathBuf::from("/repo"));
    }

    #[test]
    fn main_root_is_the_common_dir_itself_when_not_named_dot_git() {
        let common = PathBuf::from("/bare.git");
        assert_eq!(main_root_of(&common), common);
    }

    #[test]
    fn meta_dir_is_wt_beside_the_common_dir() {
        assert_eq!(
            meta_dir_of(&PathBuf::from("/repo/.git")),
            PathBuf::from("/repo/.git/wt")
        );
    }

    // ------------------------------------------------------------------------- F5

    #[test]
    fn f5_symbolic_ref_keeps_slashes() {
        assert_eq!(
            default_branch_from_symbolic_ref("origin", "refs/remotes/origin/release/main\n"),
            Some("release/main".to_owned())
        );
    }

    #[test]
    fn f5_symbolic_ref_empty_output_is_none() {
        assert_eq!(default_branch_from_symbolic_ref("origin", ""), None);
    }

    #[test]
    fn f5_ls_remote_keeps_slashes() {
        let out = "ref: refs/heads/release/main\tHEAD\ndeadbeef\tHEAD\n";
        assert_eq!(
            default_branch_from_ls_remote(out),
            Some("release/main".to_owned())
        );
    }

    #[test]
    fn f5_ls_remote_with_no_symref_line_is_none() {
        assert_eq!(default_branch_from_ls_remote("deadbeef\tHEAD\n"), None);
    }

    // ---------------------------------------------------------------- provider_of_url

    #[test]
    fn test_ado_https() {
        assert_eq!(
            provider_of_url("https://org@dev.azure.com/org/project/_git/repo"),
            "ado"
        );
    }

    #[test]
    fn test_ado_ssh() {
        assert_eq!(
            provider_of_url("git@ssh.dev.azure.com:v3/org/project/repo"),
            "ado"
        );
    }

    #[test]
    fn test_github() {
        assert_eq!(provider_of_url("git@github.com:bjornkpu/wt.git"), "github");
        assert_eq!(
            provider_of_url("https://github.com/bjornkpu/wt.git"),
            "github"
        );
    }

    #[test]
    fn test_no_remote() {
        assert_eq!(provider_of_url(""), "none");
    }

    #[test]
    fn test_legacy_visualstudio_host() {
        assert_eq!(
            provider_of_url("https://myorg.visualstudio.com/proj/_git/repo"),
            "ado"
        );
    }

    #[test]
    fn test_lookalike_host_is_not_ado() {
        assert_eq!(
            provider_of_url("https://example.com/dev.azure.com/repo.git"),
            "none"
        );
    }

    // -------------------------------------------------------------------------- F9

    #[test]
    fn f9_a_non_object_json_is_reported_not_object() {
        for text in ["[1, 2]", "\"x\"", "42", "null"] {
            assert!(matches!(parse_meta(text), SidecarRead::NotObject), "{text}");
        }
    }

    #[test]
    fn f9_invalid_json_is_skipped_silently() {
        assert!(matches!(parse_meta("{not json"), SidecarRead::Invalid));
    }

    #[test]
    fn f9_a_well_formed_object_decodes() {
        let text = r#"{"mode":"new","path":"/x","created":"2026-09-01T00:00:00+00:00"}"#;
        let SidecarRead::Meta(meta) = parse_meta(text) else {
            panic!("expected Meta");
        };
        assert_eq!(meta.mode.as_deref(), Some("new"));
        assert_eq!(meta.path.as_deref(), Some("/x"));
    }

    #[test]
    fn f9_a_mistyped_field_is_ignored_not_the_whole_sidecar() {
        // A non-string `created`, a non-array `copied`, a non-array
        // `exec_failed`: none of this used to decode at all (the derived
        // Deserialize failed the whole object), silently losing `mode` too.
        let text = r#"{"mode":"new","created":123,"copied":null,"exec_failed":true}"#;
        let SidecarRead::Meta(meta) = parse_meta(text) else {
            panic!("expected Meta, a mistyped field must not drop the sidecar");
        };
        assert_eq!(meta.mode.as_deref(), Some("new"));
        assert!(meta.copied.is_empty());
        assert!(meta.exec_failed.is_empty());
        // Present but not a timestamp: age_str (F9) must still show "?", not
        // "" (which means "no created key at all").
        assert_eq!(
            crate::domain::view::age_str(meta.created.as_deref(), jiff::Timestamp::now()),
            "?"
        );
    }

    // ----------------------------------------------------------- sidecar writing

    #[test]
    fn sidecar_json_is_byte_compatible_with_python() {
        // Expected text from Python 3.13: json.dumps(m, indent=2) for the
        // same dict, keys in the order create() builds it.
        let meta = Meta {
            mode: Some("new".to_owned()),
            path: Some(r"C:\r\blåbær.worktrees\feat\x".to_owned()),
            branch: None,
            base: Some("origin/main".to_owned()),
            detached: Some(false),
            copied: vec![],
            created: Some("2026-09-28T10:11:12.123456+00:00".to_owned()),
            id: Some("7".to_owned()),
            exec_failed: vec!["echo \u{1F600} \u{7f} \"q\"".to_owned()],
            ..Meta::default()
        };
        let python = r#"{
  "mode": "new",
  "path": "C:\\r\\bl\u00e5b\u00e6r.worktrees\\feat\\x",
  "branch": null,
  "base": "origin/main",
  "detached": false,
  "copied": [],
  "created": "2026-09-28T10:11:12.123456+00:00",
  "id": "7",
  "exec_failed": [
    "echo \ud83d\ude00 \u007f \"q\""
  ]
}"#;
        let expected = if cfg!(windows) {
            python.replace('\n', "\r\n")
        } else {
            python.to_owned()
        };
        assert_eq!(sidecar_json(&meta).unwrap(), expected);
    }

    #[test]
    fn a_written_sidecar_reads_back() {
        let meta = Meta {
            mode: Some("new".to_owned()),
            branch: Some("feat/x".to_owned()),
            copied: vec![".env".to_owned()],
            ..Meta::default()
        };
        let SidecarRead::Meta(back) = parse_meta(&sidecar_json(&meta).unwrap()) else {
            panic!("expected Meta");
        };
        assert_eq!(back.branch.as_deref(), Some("feat/x"));
        assert_eq!(back.copied, [".env"]);
    }

    #[test]
    fn sidecar_file_name_matches_urllib_quote() {
        // urllib.parse.quote('feat/x ø~_.-+', safe='')
        assert_eq!(
            sidecar_file_name("feat/x ø~_.-+"),
            "feat%2Fx%20%C3%B8~_.-%2B.json"
        );
    }

    #[test]
    fn iso_utc_matches_python_isoformat() {
        let ts: jiff::Timestamp = "2026-09-28T10:11:12.123456Z".parse().unwrap();
        assert_eq!(iso_utc(ts), "2026-09-28T10:11:12.123456+00:00");
        let whole: jiff::Timestamp = "2026-09-28T10:11:12Z".parse().unwrap();
        assert_eq!(iso_utc(whole), "2026-09-28T10:11:12+00:00");
        let ms: jiff::Timestamp = "2026-09-28T10:11:12.5Z".parse().unwrap();
        assert_eq!(iso_utc(ms), "2026-09-28T10:11:12.500000+00:00");
    }

    #[test]
    fn f9_a_missing_created_key_stays_absent() {
        let SidecarRead::Meta(meta) = parse_meta(r#"{"mode":"new"}"#) else {
            panic!("expected Meta");
        };
        assert_eq!(meta.created, None);
    }

    // -------------------------------------------------------- parse_hook_payload

    #[test]
    fn empty_stdin_is_an_empty_object() {
        assert_eq!(parse_hook_payload(b"").unwrap(), serde_json::Map::new());
        assert_eq!(
            parse_hook_payload(b"   \n").unwrap(),
            serde_json::Map::new()
        );
    }

    #[test]
    fn a_well_formed_object_decodes() {
        let obj = parse_hook_payload(br#"{"name":"x"}"#).unwrap();
        assert_eq!(
            obj.get("name").and_then(serde_json::Value::as_str),
            Some("x")
        );
    }

    #[test]
    fn invalid_json_is_refused() {
        let err = parse_hook_payload(b"{not json").unwrap_err();
        assert_eq!(err.to_string(), "hook payload was not JSON");
    }

    #[test]
    fn a_json_array_is_refused_with_its_type_name() {
        let err = parse_hook_payload(b"[]").unwrap_err();
        assert_eq!(
            err.to_string(),
            "hook payload was a JSON list, expected an object"
        );
    }

    #[test]
    fn other_non_object_json_types_name_themselves() {
        for (text, kind) in [("null", "NoneType"), ("42", "int"), ("\"x\"", "str")] {
            let err = parse_hook_payload(text.as_bytes()).unwrap_err();
            assert_eq!(
                err.to_string(),
                format!("hook payload was a JSON {kind}, expected an object")
            );
        }
    }
}
