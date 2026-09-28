use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::domain::{parse, plan};
use crate::error::AppError;

/// Error excerpts from a CLI stay this short, as in the Python tool.
const ERR_EXCERPT: usize = 400;

/// The forge this checkout's remote implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Provider {
    /// `None`: an ADO host whose path did not parse. A PR lookup runs
    /// without `--org`, a work item lookup is refused (its URL needs the
    /// org and project), and writes are skipped.
    Ado(Option<Ado>),
    /// `gh` finds the repo from its cwd, so nothing is parsed out of the URL.
    GitHub,
    None,
}

/// An ADO repo, decoded from the remote URL and checked against
/// Unicode alphanumerics plus `._ -` (ADO names may hold æøå).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ado {
    pub org: String,
    pub project: String,
    pub repo: String,
    /// `https://dev.azure.com`, or `https://<org>.visualstudio.com`.
    pub base: String,
}

/// A PR's head: the sha to detach at and the branch it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrHead {
    pub sha: String,
    pub branch: String,
}

/// A tracker item's title and its tracker-side type (a label or work item
/// type, lowercased), before the type map turns it into a branch type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    pub typ: String,
}

/// The built-in `[naming.type_from_tracker]`.
const TYPE_FROM_TRACKER: [(&str, &str); 6] = [
    ("bug", "fix"),
    ("task", "chore"),
    ("user story", "feat"),
    ("product backlog item", "feat"),
    ("feature", "feat"),
    ("epic", "feat"),
];

/// The provider `url` implies. Refuses an org/project/repo that, decoded,
/// holds anything but Unicode alphanumerics and `._ -` (F2): they end up in
/// az calls. M9 must send project/repo only percent-encoded in `az rest`
/// URLs, never as az argv, which mangles æøå.
pub fn from_url(url: &str) -> Result<Provider, AppError> {
    match parse::provider_of_url(url) {
        "github" => Ok(Provider::GitHub),
        "ado" => {
            let Some((org, project, repo, legacy)) = ado_parts(url) else {
                return Ok(Provider::Ado(None));
            };
            let org = checked(org)?;
            let base = if legacy {
                format!("https://{org}.visualstudio.com")
            } else {
                "https://dev.azure.com".to_owned()
            };
            Ok(Provider::Ado(Some(Ado {
                project: checked(project)?,
                repo: checked(repo)?,
                org,
                base,
            })))
        }
        _ => Ok(Provider::None),
    }
}

/// Org, project and repo (still encoded), and whether the org is a legacy
/// `<org>.visualstudio.com` account. `None` when the path is no shape ADO
/// uses.
fn ado_parts(url: &str) -> Option<(&str, &str, &str, bool)> {
    let (host, path) = parse::split_remote(url)?;
    let lower = host.to_ascii_lowercase();
    let path = path.strip_suffix(".git").unwrap_or(path);
    let segs: Vec<&str> = path.split('/').collect();
    let parts = if [
        "dev.azure.com",
        "ssh.dev.azure.com",
        "vs-ssh.visualstudio.com",
    ]
    .contains(&lower.as_str())
    {
        let segs = match segs.as_slice() {
            ["v3", rest @ ..] => rest,
            all => all,
        };
        let (org, project, repo) = match segs {
            [o, p, "_git", r] | [o, p, r] => (*o, *p, *r),
            _ => return None,
        };
        (org, project, repo, lower.starts_with("vs-ssh."))
    } else {
        let org_len = lower.strip_suffix(".visualstudio.com")?.len();
        // The org in the URL's own case; `lower` only found where it ends.
        let org = host.get(..org_len)?;
        if org.contains('.') {
            return None;
        }
        let (project, repo) = match segs.as_slice() {
            [dc, p, "_git", r] if dc.eq_ignore_ascii_case("DefaultCollection") => (*p, *r),
            [p, "_git", r] => (*p, *r),
            _ => return None,
        };
        (org, project, repo, true)
    };
    let (org, project, repo, _) = parts;
    [org, project, repo]
        .iter()
        .all(|s| !s.is_empty())
        .then_some(parts)
}

fn checked(part: &str) -> Result<String, AppError> {
    let decoded = percent_encoding::percent_decode_str(part)
        .decode_utf8_lossy()
        .into_owned();
    let safe = !decoded.is_empty()
        && decoded
            .chars()
            .all(|c| c.is_alphanumeric() || "._ -".contains(c));
    if safe {
        Ok(decoded)
    } else {
        Err(AppError::UnsafeRemotePart(decoded))
    }
}

impl Ado {
    /// The URL that addresses this organisation: the org is a path segment
    /// under dev.azure.com but the hostname itself on visualstudio.com, and
    /// appending it there too makes ADO read it as a collection name.
    #[must_use]
    pub fn account(&self) -> String {
        if self.base.ends_with(".visualstudio.com") {
            self.base.clone()
        } else {
            format!("{}/{}", self.base, parse::quote(&self.org))
        }
    }

    /// Project and repo only ever reach az percent-encoded in a URL: az
    /// mangles æøå in its argv.
    fn project_url(&self) -> String {
        format!("{}/{}", self.account(), parse::quote(&self.project))
    }

    /// The work item a PATCH goes to.
    #[must_use]
    pub fn work_item_url(&self, id: &str) -> String {
        format!(
            "{}/_apis/wit/workitems/{}?api-version=7.1",
            self.project_url(),
            parse::quote(id)
        )
    }

    /// The work item's title and type.
    #[must_use]
    pub fn work_item_fields_url(&self, id: &str) -> String {
        format!(
            "{}/_apis/wit/workitems/{}?fields=System.Title,System.WorkItemType&api-version=7.1",
            self.project_url(),
            parse::quote(id)
        )
    }

    /// The repository, for its id and its project's id (`repo_ids`).
    #[must_use]
    pub fn repo_url(&self) -> String {
        format!(
            "{}/_apis/git/repositories/{}?api-version=7.1",
            self.project_url(),
            parse::quote(&self.repo)
        )
    }
}

/// The fixed AAD resource id for Azure DevOps, the same for every
/// organisation. Without it `az rest` warns, changes nothing, and exits 0.
const ADO_RESOURCE: &str = "499b84ac-1321-427f-aa17-267ca6975798";

/// `az rest` against ADO: a GET, or with `body_file` a JSON-patch PATCH. The
/// reply always goes to `out_file`, never stdout: az's isolated Python
/// writes a pipe in the console codepage, which mangles æøå and cannot
/// encode everything a title may hold.
#[must_use]
pub fn az_rest_argv(url: &str, out_file: &str, body_file: Option<&str>) -> Vec<String> {
    let method = if body_file.is_some() { "patch" } else { "get" };
    let mut argv = owned(&[
        "az",
        "rest",
        "--method",
        method,
        "--url",
        url,
        "--resource",
        ADO_RESOURCE,
        "--output-file",
        out_file,
        "--only-show-errors",
    ]);
    if let Some(body) = body_file {
        argv.extend(owned(&[
            "--headers",
            "Content-Type=application/json-patch+json",
            "--body",
        ]));
        argv.push(format!("@{body}"));
    }
    argv
}

/// A JSON string literal, ASCII-escaped.
fn json_str(s: &str) -> String {
    parse::ensure_ascii(&Value::from(s).to_string())
}

/// The patch that adds a Branch `ArtifactLink`: a vstfs ref of
/// project/repo/GB<branch>, each separator `%2F` (branch slashes too).
#[must_use]
pub fn link_patch(project_id: &str, repo_id: &str, branch: &str) -> String {
    let url = format!(
        "vstfs:///Git/Ref/{project_id}%2F{repo_id}%2FGB{}",
        parse::quote(branch)
    );
    format!(
        r#"[{{"op":"add","path":"/relations/-","value":{{"rel":"ArtifactLink","url":{},"attributes":{{"name":"Branch"}}}}}}]"#,
        json_str(&url)
    )
}

/// The patch that sets `System.State`.
#[must_use]
pub fn state_patch(state: &str) -> String {
    format!(
        r#"[{{"op":"add","path":"/fields/System.State","value":{}}}]"#,
        json_str(state)
    )
}

/// The work item out of the fields GET's reply, its type lowercased.
#[must_use]
pub fn work_item(reply: &Map<String, Value>) -> Item {
    let field = |key: &str| {
        reply
            .get("fields")
            .and_then(|f| f.get(key))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Item {
        title: field("System.Title"),
        typ: field("System.WorkItemType").to_lowercase(),
    }
}

/// `az rest` calls made before giving up on a transient failure.
const RETRY_ATTEMPTS: u32 = 3;

/// Calls `call` until `again` no longer holds of its reply, at most
/// `RETRY_ATTEMPTS` times, `wait`ing between attempts with the retry's
/// number (0 first). Safe to repeat: setting a state is idempotent, and ADO
/// refuses a duplicate relation with `RelationAlreadyExistsException`, which
/// `already_linked` reads as success (inventory #44: if ADO ever stops
/// refusing duplicates, this starts duplicating links).
pub fn retry<T, E>(
    mut call: impl FnMut() -> Result<T, E>,
    again: impl Fn(&T) -> bool,
    mut wait: impl FnMut(u32),
) -> Result<T, E> {
    let mut out = call()?;
    for n in 0..RETRY_ATTEMPTS.saturating_sub(1) {
        if !again(&out) {
            break;
        }
        wait(n);
        out = call()?;
    }
    Ok(out)
}

/// The pause before retry `n`: `0.5 * 2^n * (1 + jitter)`, `jitter` in
/// `[0, 1)`.
#[must_use]
pub fn backoff(n: u32, jitter: f64) -> std::time::Duration {
    let exp = i32::try_from(n).unwrap_or(i32::MAX);
    std::time::Duration::from_secs_f64(0.5 * 2f64.powi(exp) * (1.0 + jitter))
}

/// HTTP reason phrases worth a retry: 429 and the 5xx a gateway or an
/// overloaded server answers.
const TRANSIENT_REASONS: [&str; 5] = [
    "Too Many Requests",
    "Internal Server Error",
    "Bad Gateway",
    "Service Unavailable",
    "Gateway Timeout",
];

/// Whether a failed `az rest`'s stderr is worth another attempt: a
/// transport failure or a 429/5xx, never a 4xx (the request itself is
/// wrong) or a login problem. Only the part before the reply body is read,
/// since a body may say anything.
#[must_use]
pub fn transient(stderr: &str) -> bool {
    let Some(message) = stderr
        .lines()
        .find_map(|l| l.trim().strip_prefix("ERROR:"))
        .map(str::trim)
    else {
        return false;
    };
    // requests' ConnectionError from a dropped socket: "('Connection
    // aborted.', ...)".
    if message.starts_with("('Connection aborted") {
        return true;
    }
    let head = message.split('(').next().unwrap_or_default().trim();
    TRANSIENT_REASONS.contains(&head)
        || head.ends_with("ConnectionPool")
        || head.contains("timed out")
}

/// ADO's error prose is tenant-localized; `typeKey` is not. `text` is a
/// reply body, or az's `ERROR: {reason}({body})`; one that holds no JSON
/// object falls back to a substring match.
#[must_use]
pub fn already_linked(text: &str) -> bool {
    let json = text
        .find('{')
        .zip(text.rfind('}'))
        .and_then(|(start, end)| text.get(start..=end))
        .and_then(|body| serde_json::from_str::<Value>(body).ok());
    json.map_or_else(
        || text.contains("RelationAlreadyExists"),
        |v| v.get("typeKey").and_then(Value::as_str) == Some("RelationAlreadyExistsException"),
    )
}

/// The result of a step that can fail without ending the run. Skipped is
/// its own state: "this forge has no equivalent" is not a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    pub skipped: bool,
    pub detail: String,
}

impl Outcome {
    #[must_use]
    pub fn ok(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            skipped: false,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn failed(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            skipped: false,
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn skipped(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            skipped: true,
            detail: detail.into(),
        }
    }

    /// A failure, rather than a success or a skip.
    #[must_use]
    pub const fn is_failure(&self) -> bool {
        !self.ok && !self.skipped
    }
}

/// `text` trimmed to the error excerpt length the Python tool used.
#[must_use]
pub fn excerpt(text: &str) -> String {
    text.trim().chars().take(ERR_EXCERPT).collect()
}

/// Whether a PATCH through `az rest` landed, never a silent success: az
/// exits 0 on any status below 400, so a sign-in page, an empty reply or
/// the no-op without `--resource` would pass on the exit code alone. Only
/// the updated work item (a JSON object with an `id`) counts. `Err` is the
/// detail: az's stderr, or what came back instead.
pub fn patch_sent(success: bool, reply: &str, stderr: &str) -> Result<(), String> {
    if !success {
        return Err(stderr.to_owned());
    }
    if json_object("az", true, reply, "").is_ok_and(|item| item.contains_key("id")) {
        return Ok(());
    }
    let got = if reply.trim().is_empty() {
        "an empty reply".to_owned()
    } else {
        excerpt(reply)
    };
    Err(format!("az rest did not return the work item: {got}"))
}

/// `link_branch`'s outcome from the PATCH's result (`Err`: az's stderr).
#[must_use]
pub fn link_outcome(id: &str, branch: &str, sent: &Result<(), String>) -> Outcome {
    match sent {
        Ok(()) => Outcome::ok(format!("linked branch {branch} to {id}")),
        Err(detail) if already_linked(detail) => {
            Outcome::ok(format!("branch {branch} was already linked to {id}"))
        }
        Err(detail) => Outcome::failed(format!("link_branch failed: {}", excerpt(detail))),
    }
}

/// `set_state`'s outcome from the PATCH's result (`Err`: az's stderr).
#[must_use]
pub fn state_outcome(id: &str, state: &str, sent: &Result<(), String>) -> Outcome {
    match sent {
        Ok(()) => Outcome::ok(format!("{id} -> {state}")),
        Err(detail) => Outcome::failed(format!("set_state failed: {}", excerpt(detail))),
    }
}

/// `az <args> --only-show-errors`, plus `--org` when the org is known.
fn az(args: &[&str], ado: Option<&Ado>) -> Vec<String> {
    let mut cmd: Vec<String> = std::iter::once("az")
        .chain(args.iter().copied())
        .chain(["--only-show-errors"])
        .map(str::to_owned)
        .collect();
    if let Some(ado) = ado {
        cmd.extend(["--org".to_owned(), ado.account()]);
    }
    cmd
}

fn owned(argv: &[&str]) -> Vec<String> {
    argv.iter().map(|s| (*s).to_owned()).collect()
}

impl Provider {
    /// The command that looks up PR `id`'s head, or the refusal when this
    /// forge has none.
    pub fn pr_argv(&self, id: &str) -> Result<Vec<String>, AppError> {
        match self {
            Self::Ado(ado) => Ok(az(
                &["repos", "pr", "show", "--id", id, "-o", "json"],
                ado.as_ref(),
            )),
            Self::GitHub => Ok(owned(&[
                "gh",
                "pr",
                "view",
                id,
                "--json",
                "headRefOid,headRefName",
            ])),
            Self::None => Err(AppError::Cli(
                "no supported provider on origin; cannot resolve a PR id".to_owned(),
            )),
        }
    }

    /// Why a work item cannot be looked up here, if it cannot: no tracker,
    /// or an ADO remote whose org/project did not parse (the lookup URL
    /// needs both).
    #[must_use]
    pub fn item_refusal(&self) -> Option<AppError> {
        let why = match self {
            Self::None => "no supported provider on origin",
            Self::Ado(None) => "could not parse org/project from origin",
            Self::Ado(Some(_)) | Self::GitHub => return None,
        };
        Some(AppError::Cli(format!(
            "{why}; cannot resolve a work item id"
        )))
    }

    /// The ADO repo `what` (`link_branch`, `set_state`) writes to, or its
    /// skipped outcome on a forge that cannot do it.
    pub fn write_target(&self, what: &str) -> Result<&Ado, Outcome> {
        let detail = match self {
            Self::Ado(Some(ado)) => return Ok(ado),
            Self::Ado(None) => format!("{what}: could not parse org/project from origin"),
            Self::GitHub => format!("{what}: no github equivalent, skipped"),
            Self::None => format!("{what}: no none equivalent, skipped"),
        };
        Err(Outcome::skipped(detail))
    }

    /// PR `id`'s head out of `pr_argv`'s reply.
    pub fn pr_head(&self, id: &str, reply: &Map<String, Value>) -> Result<PrHead, AppError> {
        let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_owned();
        let (sha, branch, missing) = if matches!(self, Self::GitHub) {
            (
                text(reply.get("headRefOid")),
                text(reply.get("headRefName")),
                "gh returned no head commit",
            )
        } else {
            (
                text(
                    reply
                        .get("lastMergeSourceCommit")
                        .and_then(|c| c.get("commitId")),
                ),
                text(reply.get("sourceRefName")).replace("refs/heads/", ""),
                "Azure DevOps has not computed the merge commit yet; retry shortly",
            )
        };
        if sha.is_empty() {
            return Err(AppError::Cli(format!("PR {id}: {missing}")));
        }
        // It goes to git as an argument next: never a flag or a ref name.
        if !plan::is_sha(&sha) {
            let tool = if matches!(self, Self::GitHub) {
                "gh"
            } else {
                "az"
            };
            return Err(AppError::Cli(format!(
                "PR {id}: {tool} returned a head that is not a commit sha: {sha}"
            )));
        }
        Ok(PrHead { sha, branch })
    }

    /// F13: the ref a GitHub PR lives under, so a fork's head is fetched
    /// before `worktree add` needs it. Only for a numeric id: anything else
    /// would be a refspec of the caller's choosing.
    #[must_use]
    pub fn pull_refspec(&self, id: &str) -> Option<String> {
        let numeric = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
        (matches!(self, Self::GitHub) && numeric).then(|| format!("refs/pull/{id}/head"))
    }
}

#[must_use]
pub fn gh_issue_argv(id: &str) -> Vec<String> {
    owned(&["gh", "issue", "view", id, "--json", "title,labels"])
}

/// The repo and project ids out of `az repos show`'s reply.
#[must_use]
pub fn repo_ids(reply: &Map<String, Value>) -> Option<(String, String)> {
    let repo = reply.get("id").and_then(Value::as_str)?;
    let project = reply
        .get("project")
        .and_then(|p| p.get("id"))
        .and_then(Value::as_str)?;
    Some((repo.to_owned(), project.to_owned()))
}

/// The built-in tracker type map with `[naming.type_from_tracker]` over it.
#[must_use]
pub fn type_map(cfg: &toml::Table) -> BTreeMap<String, String> {
    let mut map: BTreeMap<String, String> = TYPE_FROM_TRACKER
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    let configured = cfg
        .get("naming")
        .and_then(|n| n.get("type_from_tracker"))
        .and_then(toml::Value::as_table);
    for (k, v) in configured.into_iter().flatten() {
        if let Some(v) = v.as_str() {
            map.insert(k.clone(), v.to_owned());
        }
    }
    map
}

/// The issue out of `gh issue view`'s reply. The type is the first label,
/// in gh's order, that `type_map` knows (F4: deterministic), else
/// `feature`. Whole label names: "debug" is not a bug.
#[must_use]
pub fn gh_issue(reply: &Map<String, Value>, type_map: &BTreeMap<String, String>) -> Item {
    let typ = reply
        .get("labels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|l| l.get("name").and_then(Value::as_str))
        .map(str::to_lowercase)
        .find(|n| type_map.contains_key(n))
        .unwrap_or_else(|| "feature".to_owned());
    Item {
        title: reply
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        typ,
    }
}

/// A CLI's reply as a JSON object, or Python `run_json`'s refusal: every
/// caller reaches for a key, so a list or a scalar is a wrong answer.
pub fn json_object(
    tool: &str,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<Map<String, Value>, AppError> {
    if !success {
        let text = if stderr.is_empty() { stdout } else { stderr };
        return Err(AppError::Cli(format!("{tool} failed\n{}", text.trim())));
    }
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        let excerpt = excerpt(stdout);
        return Err(AppError::Cli(format!(
            "{tool} returned non-JSON:\n{excerpt}"
        )));
    };
    let kind = match value {
        Value::Object(map) => return Ok(map),
        Value::Array(_) => "list",
        Value::String(_) => "str",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::Bool(_) => "bool",
        Value::Null => "NoneType",
    };
    Err(AppError::Cli(format!(
        "{tool} returned a JSON {kind}, expected an object"
    )))
}

/// cp1252's 0x80..=0x9F; 0 where cp1252 leaves the byte undefined.
const CP1252_HIGH: [u16; 32] = [
    0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC,
    0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178,
];

/// A CLI's output as text: utf-8, else cp1252 (a tool that ignored the
/// utf-8 env vars and wrote the console codepage), else latin-1, which
/// cannot fail and so guesses.
#[must_use]
pub fn decode(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let cp1252: Option<String> = bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9F => CP1252_HIGH
                .get(usize::from(b.wrapping_sub(0x80)))
                .filter(|&&c| c != 0)
                .and_then(|&c| char::from_u32(u32::from(c))),
            _ => Some(char::from(b)),
        })
        .collect();
    cp1252.unwrap_or_else(|| bytes.iter().map(|&b| char::from(b)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ado(org: &str, project: &str, repo: &str, base: &str) -> Provider {
        Provider::Ado(Some(Ado {
            org: org.to_owned(),
            project: project.to_owned(),
            repo: repo.to_owned(),
            base: base.to_owned(),
        }))
    }

    fn obj(json: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(json).unwrap()
    }

    // ------------------------------------------------------------- TestDecode

    #[test]
    fn test_prefers_utf8() {
        assert_eq!(decode("går".as_bytes()), "går");
    }

    #[test]
    fn test_falls_back_for_console_codepage() {
        // "går" in cp1252; 0x80 is the euro sign there, not a latin-1 control.
        assert_eq!(decode(b"g\xe5r \x80"), "går €");
    }

    #[test]
    fn test_undecodable_bytes_do_not_raise() {
        // 0x81 is undefined in cp1252, so latin-1 takes it.
        assert_eq!(decode(b"\xff\xfe\x00\x81"), "\u{ff}\u{fe}\u{0}\u{81}");
    }

    // --------------------------------------------------------- TestProviderOf

    #[test]
    fn test_ado_https() {
        assert_eq!(
            from_url("https://contoso@dev.azure.com/contoso/Prosjekt/_git/repo").unwrap(),
            ado("contoso", "Prosjekt", "repo", "https://dev.azure.com")
        );
    }

    #[test]
    fn test_ado_ssh() {
        assert_eq!(
            from_url("git@ssh.dev.azure.com:v3/contoso/Prosjekt/repo").unwrap(),
            ado("contoso", "Prosjekt", "repo", "https://dev.azure.com")
        );
    }

    #[test]
    fn test_github() {
        assert_eq!(
            from_url("https://github.com/owner/repo.git").unwrap(),
            Provider::GitHub
        );
        assert_eq!(
            from_url("git@github.com:owner/repo.git").unwrap(),
            Provider::GitHub
        );
    }

    #[test]
    fn test_no_remote() {
        assert_eq!(from_url("").unwrap(), Provider::None);
        assert_eq!(from_url("/tmp/origin.git").unwrap(), Provider::None);
    }

    #[test]
    fn test_legacy_visualstudio_host() {
        assert_eq!(
            from_url("https://myorg.visualstudio.com/myproject/_git/myrepo").unwrap(),
            ado(
                "myorg",
                "myproject",
                "myrepo",
                "https://myorg.visualstudio.com"
            )
        );
    }

    #[test]
    fn f3_legacy_default_collection_and_vs_ssh_parse() {
        let want = ado("myorg", "proj", "repo", "https://myorg.visualstudio.com");
        for url in [
            "https://myorg.visualstudio.com/DefaultCollection/proj/_git/repo",
            "https://myorg@myorg.visualstudio.com/defaultcollection/proj/_git/repo.git",
            "myorg@vs-ssh.visualstudio.com:v3/myorg/proj/repo",
            "ssh://myorg@vs-ssh.visualstudio.com:22/v3/myorg/proj/repo",
        ] {
            assert_eq!(from_url(url).unwrap(), want, "{url}");
        }
    }

    #[test]
    fn f2_percent_encoded_parts_are_decoded_before_the_check() {
        assert_eq!(
            from_url("https://o@dev.azure.com/o/My%20Project/_git/my%20repo").unwrap(),
            ado("o", "My Project", "my repo", "https://dev.azure.com")
        );
        assert_eq!(
            from_url("https://contoso@dev.azure.com/contoso/%C3%98konomiProsjekt/_git/repo")
                .unwrap(),
            ado(
                "contoso",
                "ØkonomiProsjekt",
                "repo",
                "https://dev.azure.com"
            )
        );
        for bad in ["o%22x", "o%5Cx", "o%25x", "o%0Ax", "o'x"] {
            assert!(
                from_url(&format!("https://dev.azure.com/o/{bad}/_git/r")).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn test_an_ado_host_with_an_unparseable_path_has_no_org() {
        for url in [
            "https://dev.azure.com/only-one-segment",
            "https://dev.azure.com//p/r",
            "https://a.b.visualstudio.com/p/_git/r",
        ] {
            assert_eq!(from_url(url).unwrap(), Provider::Ado(None), "{url}");
        }
    }

    #[test]
    fn test_shell_metacharacters_in_the_org_are_refused() {
        let err = from_url("https://x@dev.azure.com/o;whoami/p/_git/r").unwrap_err();
        assert_eq!(
            err.to_string(),
            "refusing to use 'o;whoami' from the origin URL"
        );
        // An encoded slash decodes into something the check refuses too.
        assert!(from_url("https://dev.azure.com/o/p%2F..%2Fx/_git/r").is_err());
    }

    #[test]
    fn test_lookalike_host_is_not_ado() {
        assert_eq!(
            from_url("https://evil.example.com/dev.azure.com/a/b/_git/c").unwrap(),
            Provider::None
        );
    }

    #[test]
    fn test_account_url_does_not_repeat_a_legacy_org() {
        let Provider::Ado(Some(legacy)) =
            ado("myorg", "proj", "r", "https://myorg.visualstudio.com")
        else {
            panic!()
        };
        assert_eq!(legacy.account(), "https://myorg.visualstudio.com");
        let Provider::Ado(Some(modern)) = ado("my org", "proj", "r", "https://dev.azure.com")
        else {
            panic!()
        };
        assert_eq!(modern.account(), "https://dev.azure.com/my%20org");
    }

    #[test]
    fn test_unsupported_provider_refuses_lookups() {
        assert_eq!(
            Provider::None.pr_argv("1").unwrap_err().to_string(),
            "no supported provider on origin; cannot resolve a PR id"
        );
    }

    // ------------------------------------------------------------------- argv

    #[test]
    fn ado_pr_show_argv_carries_the_org_when_parsed() {
        let p = ado("myorg", "proj", "r", "https://myorg.visualstudio.com");
        assert_eq!(
            p.pr_argv("42").unwrap(),
            [
                "az",
                "repos",
                "pr",
                "show",
                "--id",
                "42",
                "-o",
                "json",
                "--only-show-errors",
                "--org",
                "https://myorg.visualstudio.com"
            ]
        );
        assert_eq!(
            Provider::Ado(None).pr_argv("42").unwrap(),
            [
                "az",
                "repos",
                "pr",
                "show",
                "--id",
                "42",
                "-o",
                "json",
                "--only-show-errors"
            ]
        );
    }

    #[test]
    fn gh_pr_view_argv() {
        assert_eq!(
            Provider::GitHub.pr_argv("7").unwrap(),
            ["gh", "pr", "view", "7", "--json", "headRefOid,headRefName"]
        );
    }

    fn spaced_ado() -> Ado {
        let Provider::Ado(Some(a)) = ado(
            "contoso org",
            "Økonomi Prosjekt",
            "repo å",
            "https://dev.azure.com",
        ) else {
            panic!()
        };
        a
    }

    #[test]
    fn f2_ado_urls_percent_encode_project_repo_and_id() {
        let a = spaced_ado();
        assert_eq!(
            a.work_item_url("21438"),
            "https://dev.azure.com/contoso%20org/%C3%98konomi%20Prosjekt/_apis/wit/workitems/21438?api-version=7.1"
        );
        assert_eq!(
            a.work_item_fields_url("1/2"),
            "https://dev.azure.com/contoso%20org/%C3%98konomi%20Prosjekt/_apis/wit/workitems/1%2F2?fields=System.Title,System.WorkItemType&api-version=7.1"
        );
        assert_eq!(
            a.repo_url(),
            "https://dev.azure.com/contoso%20org/%C3%98konomi%20Prosjekt/_apis/git/repositories/repo%20%C3%A5?api-version=7.1"
        );
    }

    #[test]
    fn az_rest_get_carries_the_resource_and_an_output_file() {
        assert_eq!(
            az_rest_argv("https://x/y?a=1", "out.json", None),
            [
                "az",
                "rest",
                "--method",
                "get",
                "--url",
                "https://x/y?a=1",
                "--resource",
                "499b84ac-1321-427f-aa17-267ca6975798",
                "--output-file",
                "out.json",
                "--only-show-errors"
            ]
        );
    }

    #[test]
    fn az_rest_patch_sends_the_body_file_as_json_patch() {
        assert_eq!(
            az_rest_argv("https://x/y", "out.json", Some("body.json")),
            [
                "az",
                "rest",
                "--method",
                "patch",
                "--url",
                "https://x/y",
                "--resource",
                "499b84ac-1321-427f-aa17-267ca6975798",
                "--output-file",
                "out.json",
                "--only-show-errors",
                "--headers",
                "Content-Type=application/json-patch+json",
                "--body",
                "@body.json"
            ]
        );
    }

    #[test]
    fn link_patch_is_the_python_artifact_link() {
        assert_eq!(
            link_patch("p1", "r1", "fix/21438-msal-login-loop"),
            r#"[{"op":"add","path":"/relations/-","value":{"rel":"ArtifactLink","url":"vstfs:///Git/Ref/p1%2Fr1%2FGBfix%2F21438-msal-login-loop","attributes":{"name":"Branch"}}}]"#
        );
    }

    #[test]
    fn state_patch_is_ascii_escaped_json() {
        assert_eq!(
            state_patch("Active"),
            r#"[{"op":"add","path":"/fields/System.State","value":"Active"}]"#
        );
        // Python's json.dumps escapes too; the body file is then plain ASCII.
        assert_eq!(
            state_patch("Pågår \"nå\""),
            r#"[{"op":"add","path":"/fields/System.State","value":"P\u00e5g\u00e5r \"n\u00e5\""}]"#
        );
    }

    #[test]
    fn work_item_reply_gives_title_and_lowercased_type() {
        let reply = obj(
            r#"{"id":1,"fields":{"System.Title":"MSAL-innlogging går i loop","System.WorkItemType":"Bug"}}"#,
        );
        assert_eq!(
            work_item(&reply),
            Item {
                title: "MSAL-innlogging går i loop".to_owned(),
                typ: "bug".to_owned()
            }
        );
        assert_eq!(
            work_item(&obj("{}")),
            Item {
                title: String::new(),
                typ: String::new()
            }
        );
    }

    // ---------------------------------------------------- TestAdoRequestRetry
    //
    // az rest prints an HTTP failure as `ERROR: {reason}({body})` (azure-cli's
    // send_raw_request raises HTTPError(reason + "(" + text + ")")), and a
    // transport failure as requests' own message, which for urllib3 errors
    // starts with `HTTPSConnectionPool(host=..., port=443): ...`.

    const AZ_400: &str = "ERROR: Bad Request({\"$id\":\"1\",\"innerException\":null,\"message\":\"TF401232: Work item 1 does not exist.\",\"typeName\":\"Microsoft.TeamFoundation.WorkItemTracking.Server.WorkItemUnauthorizedAccessException\",\"typeKey\":\"WorkItemUnauthorizedAccessException\",\"errorCode\":0,\"eventId\":3200})\n";
    const AZ_404: &str = "ERROR: Not Found({\"$id\":\"1\",\"message\":\"VS800075: The project with id 'x' does not exist\",\"typeKey\":\"ProjectDoesNotExistWithNameException\"})\n";
    const AZ_401: &str = "ERROR: Unauthorized(<html>401 - timed out session</html>)\n";
    const AZ_429: &str = "ERROR: Too Many Requests({\"message\":\"Request was blocked due to exceeding usage of resource\"})\n";
    const AZ_500: &str = "ERROR: Internal Server Error({\"message\":\"boom\"})\n";
    const AZ_503: &str = "ERROR: Service Unavailable(<html>Service Unavailable</html>)\n";
    const AZ_DNS: &str = "ERROR: HTTPSConnectionPool(host='dev.azure.com', port=443): Max retries exceeded with url: /o/p/_apis/wit/workitems/1?api-version=7.1 (Caused by NameResolutionError(\"<urllib3.connection.HTTPSConnection object at 0x0>: Failed to resolve 'dev.azure.com' ([Errno 11001] getaddrinfo failed)\"))\n";
    const AZ_READ_TIMEOUT: &str = "ERROR: HTTPSConnectionPool(host='dev.azure.com', port=443): Read timed out. (read timeout=None)\n";
    const AZ_ABORTED: &str = "ERROR: ('Connection aborted.', ConnectionResetError(10054, 'An existing connection was forcibly closed by the remote host', None, 10054, None))\n";
    const AZ_LOGIN: &str = "ERROR: Please run 'az login' to setup account.\n";

    #[test]
    fn test_a_transient_failure_is_retried() {
        for stderr in [AZ_DNS, AZ_READ_TIMEOUT, AZ_ABORTED] {
            assert!(transient(stderr), "{stderr}");
        }
    }

    #[test]
    fn test_a_server_error_is_retried() {
        for stderr in [AZ_429, AZ_500, AZ_503] {
            assert!(transient(stderr), "{stderr}");
        }
    }

    #[test]
    fn test_a_client_error_is_not_retried() {
        // The body of a 4xx may say anything, "timed out" included: only the
        // reason phrase in front of it decides.
        for stderr in [AZ_400, AZ_404, AZ_401, AZ_LOGIN, ""] {
            assert!(!transient(stderr), "{stderr}");
        }
    }

    /// Runs `retry` over scripted az replies; returns what it settled on and
    /// how many calls and waits it made.
    fn scripted(replies: &[(bool, &str)]) -> ((bool, String), usize, Vec<u32>) {
        let mut calls = 0;
        let mut waits = Vec::new();
        let out = retry(
            || {
                let (ok, err) = replies.get(calls).copied().unwrap();
                calls = calls.saturating_add(1);
                Ok::<_, ()>((ok, err.to_owned()))
            },
            |(ok, err): &(bool, String)| !ok && transient(err),
            |n| waits.push(n),
        )
        .unwrap();
        (out, calls, waits)
    }

    #[test]
    fn test_a_blip_then_success_takes_two_calls() {
        let (out, calls, waits) = scripted(&[(false, AZ_DNS), (true, "")]);
        assert!(out.0);
        assert_eq!(calls, 2);
        assert_eq!(waits, [0]);
    }

    #[test]
    fn test_a_client_error_is_attempted_once() {
        let (out, calls, _) = scripted(&[(false, AZ_400), (false, AZ_400), (false, AZ_400)]);
        assert!(!out.0);
        assert_eq!(calls, 1);
    }

    #[test]
    fn test_it_gives_up_and_reports_the_last_failure() {
        let (out, calls, waits) = scripted(&[(false, AZ_503), (false, AZ_500), (false, AZ_DNS)]);
        assert_eq!(calls, 3);
        assert_eq!(waits, [0, 1]);
        assert_eq!(out.1, AZ_DNS);
    }

    #[test]
    fn backoff_doubles_with_jitter() {
        assert_eq!(backoff(0, 0.0), std::time::Duration::from_millis(500));
        assert_eq!(backoff(1, 0.0), std::time::Duration::from_secs(1));
        assert_eq!(backoff(1, 0.5), std::time::Duration::from_millis(1500));
    }

    // ------------------------------------------------------ TestAlreadyLinked

    #[test]
    fn test_type_key_is_recognised() {
        let body = r#"{"typeKey": "RelationAlreadyExistsException", "message": "noe gikk galt"}"#;
        assert!(already_linked(body));
        // As az rest prints it: the reason phrase, then the body in parens.
        assert!(already_linked(&format!("ERROR: Bad Request({body})\n")));
    }

    #[test]
    fn test_a_different_failure_is_not_a_success() {
        assert!(!already_linked(r#"{"typeKey": "SomethingElse"}"#));
        assert!(!already_linked(AZ_400));
    }

    #[test]
    fn test_non_json_falls_back_to_the_substring() {
        assert!(already_linked("<html>RelationAlreadyExists</html>"));
        assert!(!already_linked("<html>500</html>"));
    }

    // ------------------------------------------------------------- outcomes

    #[test]
    fn link_outcomes_match_the_python_text() {
        let b = "fix/1-x";
        assert_eq!(
            link_outcome("1", b, &Ok(())),
            Outcome::ok("linked branch fix/1-x to 1")
        );
        let dup = format!(
            "ERROR: Bad Request({})",
            r#"{"typeKey":"RelationAlreadyExistsException"}"#
        );
        assert_eq!(
            link_outcome("1", b, &Err(dup)),
            Outcome::ok("branch fix/1-x was already linked to 1")
        );
        assert_eq!(
            link_outcome("1", b, &Err("ERROR: Forbidden(nope)".to_owned())),
            Outcome::failed("link_branch failed: ERROR: Forbidden(nope)")
        );
        let long = "x".repeat(500);
        assert_eq!(
            link_outcome("1", b, &Err(long)).detail.len(),
            "link_branch failed: ".len() + 400
        );
    }

    #[test]
    fn a_patch_counts_only_when_the_reply_is_the_work_item() {
        assert_eq!(patch_sent(true, r#"{"id":21438,"rev":7}"#, ""), Ok(()));
        // az rest exits 0 below 400: a sign-in page, an empty 204 or the
        // no-op without --resource must not read as "linked" / "-> Active".
        for reply in ["", "<html>Sign in</html>", r#"{"value":[]}"#, "[1]"] {
            let err = patch_sent(true, reply, "").unwrap_err();
            assert!(
                err.starts_with("az rest did not return the work item"),
                "{err}"
            );
            assert_eq!(
                state_outcome("1", "Active", &patch_sent(true, reply, "")),
                Outcome::failed(format!("set_state failed: {err}"))
            );
        }
        assert_eq!(
            patch_sent(true, " \n", ""),
            Err("az rest did not return the work item: an empty reply".to_owned())
        );
        assert_eq!(
            link_outcome(
                "1",
                "fix/1-x",
                &patch_sent(true, "<html>Sign in</html>", "")
            ),
            Outcome::failed(
                "link_branch failed: az rest did not return the work item: <html>Sign in</html>"
            )
        );
        assert_eq!(
            patch_sent(false, "", "ERROR: Forbidden(x)"),
            Err("ERROR: Forbidden(x)".to_owned())
        );
    }

    #[test]
    fn state_outcomes_match_the_python_text() {
        assert_eq!(
            state_outcome("1", "Active", &Ok(())),
            Outcome::ok("1 -> Active")
        );
        assert_eq!(
            state_outcome("1", "Active", &Err(" boom\n".to_owned())),
            Outcome::failed("set_state failed: boom")
        );
    }

    #[test]
    fn forges_without_writes_skip_with_the_python_text() {
        assert_eq!(
            Provider::GitHub.write_target("link_branch"),
            Err(Outcome::skipped(
                "link_branch: no github equivalent, skipped"
            ))
        );
        assert_eq!(
            Provider::None.write_target("set_state"),
            Err(Outcome::skipped("set_state: no none equivalent, skipped"))
        );
        assert_eq!(
            Provider::Ado(None).write_target("link_branch"),
            Err(Outcome::skipped(
                "link_branch: could not parse org/project from origin"
            ))
        );
        assert_eq!(
            Provider::Ado(Some(spaced_ado())).write_target("set_state"),
            Ok(&spaced_ado())
        );
    }

    #[test]
    fn item_lookup_is_refused_without_a_tracker() {
        assert_eq!(
            Provider::None.item_refusal().unwrap().to_string(),
            "no supported provider on origin; cannot resolve a work item id"
        );
        assert_eq!(
            Provider::Ado(None).item_refusal().unwrap().to_string(),
            "could not parse org/project from origin; cannot resolve a work item id"
        );
        assert!(Provider::GitHub.item_refusal().is_none());
    }

    #[test]
    fn a_pr_head_that_is_not_a_sha_is_refused() {
        let reply = obj(r#"{"headRefOid":"--upload-pack=x","headRefName":"fix/y"}"#);
        assert_eq!(
            Provider::GitHub
                .pr_head("5", &reply)
                .unwrap_err()
                .to_string(),
            "PR 5: gh returned a head that is not a commit sha: --upload-pack=x"
        );
    }

    #[test]
    fn gh_issue_view_argv() {
        assert_eq!(
            gh_issue_argv("9"),
            ["gh", "issue", "view", "9", "--json", "title,labels"]
        );
    }

    #[test]
    fn f13_github_pr_fetches_the_pull_ref() {
        assert_eq!(
            Provider::GitHub.pull_refspec("12"),
            Some("refs/pull/12/head".to_owned())
        );
        assert_eq!(Provider::GitHub.pull_refspec("12:refs/heads/main"), None);
        assert_eq!(Provider::Ado(None).pull_refspec("12"), None);
    }

    // ---------------------------------------------------------------- replies

    #[test]
    fn run_json_errors_match_the_python_messages() {
        assert_eq!(
            json_object("az", false, "out", "  boom\n")
                .unwrap_err()
                .to_string(),
            "az failed\nboom"
        );
        assert_eq!(
            json_object("az", false, " out ", "")
                .unwrap_err()
                .to_string(),
            "az failed\nout"
        );
        assert_eq!(
            json_object("gh", true, "nope", "").unwrap_err().to_string(),
            "gh returned non-JSON:\nnope"
        );
        for (text, name) in [
            ("[1]", "list"),
            ("\"x\"", "str"),
            ("1", "int"),
            ("1.5", "float"),
            ("true", "bool"),
            ("null", "NoneType"),
        ] {
            assert_eq!(
                json_object("gh", true, text, "").unwrap_err().to_string(),
                format!("gh returned a JSON {name}, expected an object")
            );
        }
        assert_eq!(
            json_object("gh", true, "{\"a\":1}", "").unwrap(),
            obj("{\"a\":1}")
        );
    }

    #[test]
    fn ado_pr_reply_gives_the_source_head_and_branch() {
        let p = Provider::Ado(None);
        let reply = obj(
            r#"{"lastMergeSourceCommit":{"commitId":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"},"sourceRefName":"refs/heads/feat/x"}"#,
        );
        assert_eq!(
            p.pr_head("5", &reply).unwrap(),
            PrHead {
                sha: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
                branch: "feat/x".to_owned()
            }
        );
        assert_eq!(
            p.pr_head("5", &obj("{}")).unwrap_err().to_string(),
            "PR 5: Azure DevOps has not computed the merge commit yet; retry shortly"
        );
    }

    #[test]
    fn gh_pr_reply_gives_the_head() {
        let reply = obj(
            r#"{"headRefOid":"dddddddddddddddddddddddddddddddddddddddd","headRefName":"fix/y"}"#,
        );
        assert_eq!(
            Provider::GitHub.pr_head("5", &reply).unwrap(),
            PrHead {
                sha: "dddddddddddddddddddddddddddddddddddddddd".to_owned(),
                branch: "fix/y".to_owned()
            }
        );
        assert_eq!(
            Provider::GitHub
                .pr_head("5", &obj(r#"{"headRefOid":""}"#))
                .unwrap_err()
                .to_string(),
            "PR 5: gh returned no head commit"
        );
    }

    #[test]
    fn az_repos_show_reply_gives_both_ids() {
        assert_eq!(
            repo_ids(&obj(r#"{"id":"r1","project":{"id":"p1"}}"#)),
            Some(("r1".to_owned(), "p1".to_owned()))
        );
        assert_eq!(repo_ids(&obj(r#"{"id":"r1"}"#)), None);
    }

    // ------------------------------------------------------ TestGitHubTypeMap

    #[test]
    fn test_a_configured_label_is_recognised() {
        let cfg: toml::Table =
            toml::from_str("[naming.type_from_tracker]\nregresjon = \"fix\"\n").unwrap();
        let map = type_map(&cfg);
        let reply = obj(r#"{"title":"T","labels":[{"name":"Regresjon"}]}"#);
        assert_eq!(
            gh_issue(&reply, &map),
            Item {
                title: "T".to_owned(),
                typ: "regresjon".to_owned()
            }
        );
    }

    #[test]
    fn test_provider_of_carries_the_merged_map() {
        let cfg: toml::Table =
            toml::from_str("[naming.type_from_tracker]\nregresjon = \"fix\"\n").unwrap();
        let map = type_map(&cfg);
        assert_eq!(map.get("regresjon").map(String::as_str), Some("fix"));
        assert_eq!(
            map.get("bug").map(String::as_str),
            Some("fix"),
            "the built-ins must survive the merge"
        );
    }

    #[test]
    fn f4_the_first_known_label_in_gh_order_wins() {
        let map = type_map(&toml::Table::new());
        let reply = obj(r#"{"labels":[{"name":"debug"},{"name":"Feature"},{"name":"bug"}]}"#);
        assert_eq!(gh_issue(&reply, &map).typ, "feature");
        let reply = obj(r#"{"labels":[{"name":"bug"},{"name":"feature"}]}"#);
        assert_eq!(gh_issue(&reply, &map).typ, "bug");
        assert_eq!(gh_issue(&obj("{}"), &map).typ, "feature");
    }
}
