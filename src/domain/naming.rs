use crate::error::AppError;

/// The conventional types `--type` accepts (F15) and the naming model may pick.
pub const TYPES: &[&str] = &[
    "feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci",
];

/// The built-in stopword list, english then norwegian (already folded: "på"
/// is "pa" by the time slugify compares).
pub const STOPWORDS: &[&str] = &[
    "a", "an", "the", "on", "in", "of", "for", "to", "and", "or", "with", "at", "by", "is", "are",
    "when", "from", "that", "this", "it", "not", "og", "i", "pa", "naar", "na", "som", "er", "en",
    "et", "til", "av", "med", "den", "det", "de", "blir", "ved", "om", "har", "kan", "ikke",
];

/// Python's `BRANCH_SPEC`, `^[a-z0-9]+/[a-z0-9]+(?:[-/][a-z0-9]+)*$`: already
/// a branch name, taken verbatim.
#[must_use]
pub fn is_branch_spec(text: &str) -> bool {
    let Some((head, _)) = text.split_once(['-', '/']) else {
        return false;
    };
    text.strip_prefix(head)
        .is_some_and(|rest| rest.starts_with('/'))
        && text
            .split(['-', '/'])
            .all(|w| !w.is_empty() && w.chars().all(is_lower_alnum))
}

/// F15: `--slug` must be `[a-z0-9]+(-[a-z0-9]+)*`.
#[must_use]
pub fn is_slug(text: &str) -> bool {
    text.split('-')
        .all(|w| !w.is_empty() && w.chars().all(is_lower_alnum))
}

const fn is_lower_alnum(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit()
}

/// Lowercase kebab words from free text: æ/ø/å folded, stopwords dropped
/// (unless that leaves nothing), at most `words` of them, "work" if empty.
/// An empty `stopwords` means the built-in list, as in the Python tool.
#[must_use]
pub fn slugify(text: &str, words: usize, stopwords: &[String]) -> String {
    let folded = text
        .to_lowercase()
        .replace('æ', "ae")
        .replace('ø', "o")
        .replace('å', "a");
    let parts: Vec<&str> = folded
        .split(|c: char| !is_lower_alnum(c))
        .filter(|w| !w.is_empty())
        .collect();
    let is_stop = |w: &&str| {
        if stopwords.is_empty() {
            STOPWORDS.contains(w)
        } else {
            stopwords.iter().any(|s| s == w)
        }
    };
    let kept: Vec<&str> = parts.iter().copied().filter(|w| !is_stop(w)).collect();
    let chosen = if kept.is_empty() { parts } else { kept };
    let slug = chosen.into_iter().take(words).collect::<Vec<_>>().join("-");
    if slug.is_empty() {
        "work".to_owned()
    } else {
        slug
    }
}

/// F11: substitutes the `{name}` placeholders `vars` supplies. Any other
/// `{word}` (lowercase letters and `_`) is a config typo and an error; every
/// other brace is literal, so a `{ ... }` script block in `exec` survives.
/// One pass over the template, so a substituted value is never rescanned.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> Result<String, AppError> {
    let mut out = String::with_capacity(template.len());
    let mut unknown = false;
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let placeholder = after.split_once('}').filter(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        });
        let Some((name, tail)) = placeholder else {
            out.push('{');
            rest = after;
            continue;
        };
        if let Some((_, value)) = vars.iter().find(|(k, _)| *k == name) {
            out.push_str(value);
        } else {
            unknown = true;
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = tail;
    }
    out.push_str(rest);
    if unknown {
        return Err(AppError::UnresolvedPlaceholder {
            template: template.to_owned(),
            out,
        });
    }
    Ok(out)
}

/// `[naming] stopwords` from the merged config; empty (the built-in list)
/// when unset.
#[must_use]
pub fn stopwords_of(cfg: &toml::Table) -> Vec<String> {
    cfg.get("naming")
        .and_then(|n| n.get("stopwords"))
        .and_then(toml::Value::as_array)
        .map(|words| {
            words
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The default `[naming] system` prompt, built from `TYPES` so the two
/// never drift apart. Says 2-4 words while `is_model_slug` accepts 2-6, same
/// gap as the Python original.
#[must_use]
pub fn default_naming_system() -> String {
    format!(
        "Output ONLY compact JSON: {{\"type\":\"...\",\"slug\":\"...\"}}. type must be one of: {}. \
         slug must be 2-4 lowercase ENGLISH kebab-case words naming the concrete subject \
         (translate from Norwegian if needed; drop filler words). No prose, no code fence.",
        TYPES.join(" ")
    )
}

/// `claude -p`'s argv: `claude_path`, the title as the prompt, `model`,
/// text output, and `system` as the system prompt. Mirrors `llm_name`'s argv
/// exactly, since the reply parser downstream assumes this shape.
#[must_use]
pub fn claude_argv(claude_path: &str, title: &str, model: &str, system: &str) -> Vec<String> {
    vec![
        claude_path.to_owned(),
        "-p".to_owned(),
        format!("Work item title: {title}"),
        "--model".to_owned(),
        model.to_owned(),
        "--output-format".to_owned(),
        "text".to_owned(),
        "--system-prompt".to_owned(),
        system.to_owned(),
    ]
}

/// Why the naming model will not run, checked before anything is spawned:
/// `llm_name`'s first two guards (`naming.llm`/an empty title, then whether
/// `claude` is on PATH), factored out so they are testable without a
/// process. `claude_found` is the IO shell's own PATH lookup result.
#[must_use]
pub const fn model_skip_reason(llm: bool, title: &str, claude_found: bool) -> Option<&'static str> {
    if !llm || title.is_empty() {
        Some("naming model not requested")
    } else if !claude_found {
        Some("claude is not on PATH")
    } else {
        None
    }
}

/// The model's slug contract: `[a-z0-9]+(-[a-z0-9]+){1,5}`, 2 to 6 words.
fn is_model_slug(slug: &str) -> bool {
    let words: Vec<&str> = slug.split('-').collect();
    (2..=6).contains(&words.len())
        && words
            .iter()
            .all(|w| !w.is_empty() && w.chars().all(is_lower_alnum))
}

/// The first `{...}` span, greedy to the last `}` in the text: Python's
/// `re.search(r"[{].*[}]", out, re.DOTALL)`. Python strips code fences
/// first, but a ` ``` `/` ```json ` marker never contains a brace, so
/// removing it can never change which characters this greedy search finds
/// as the first `{` or the last `}` - confirmed by mutating a fence-strip
/// step into a no-op and finding no test cared (see the fix report). The
/// separate step was dropped rather than kept as untestable dead code.
fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    text.get(start..=end)
}

/// Python's `str(d.get(key, "")).lower()`: a string value verbatim, anything
/// else stringified, missing is empty - then lowercased.
fn field_lower(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    match obj.get(key) {
        Some(serde_json::Value::String(s)) => s.to_lowercase(),
        Some(other) => other.to_string().to_lowercase(),
        None => String::new(),
    }
}

/// Parses a naming model's reply: extracts the first `{...}` span (code
/// fences and all - see `extract_json`), decodes it as JSON, and validates
/// `type`/`slug`. `Err` carries the exact reason wt reports when it falls
/// back to the mechanical slug.
pub fn parse_model_reply(raw: &str) -> Result<(String, String), String> {
    let Some(json_text) = extract_json(raw.trim()) else {
        return Err("the naming model returned no JSON".to_owned());
    };
    let value: serde_json::Value = serde_json::from_str(json_text)
        .map_err(|_| "the naming model returned malformed JSON".to_owned())?;
    let obj = value
        .as_object()
        .ok_or_else(|| "the naming model returned malformed JSON".to_owned())?;
    let typ = field_lower(obj, "type");
    let slug = field_lower(obj, "slug");
    if TYPES.contains(&typ.as_str()) && is_model_slug(&slug) {
        Ok((typ, slug))
    } else {
        Err(format!(
            "the naming model proposed an unusable name ({typ}/{slug})"
        ))
    }
}

/// A branch type and slug, plus why the model did not supply them: empty
/// unless it was wanted, declined, and `naming.llm` is on (the log line is
/// suppressed otherwise, same as Python's `fell_back`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChosenName {
    pub typ: String,
    pub slug: String,
    pub fallback_reason: String,
}

/// `choose_name` plus the fallback reason, in the Python tool's precedence
/// and suppression rule: the reason is only surfaced when the model was
/// wanted, it answered nothing, and `naming.llm` is on. `model_wanted` is
/// that whole condition minus "answered nothing" (CLI flags allowed the
/// model to run, and `naming.llm` was on) - the caller's `want_model &&
/// naming.llm`, combined so this takes one flag instead of two that are
/// never used apart.
#[must_use]
pub fn chosen_name(
    text: &str,
    flags: (Option<&str>, Option<&str>),
    named: Option<(&str, &str)>,
    why: &str,
    model_wanted: bool,
    fallback_type: &str,
    stopwords: &[String],
) -> ChosenName {
    let (typ, slug) = choose_name(text, flags, named, fallback_type, stopwords);
    let fell_back = model_wanted && named.is_none();
    ChosenName {
        typ,
        slug,
        fallback_reason: if fell_back {
            why.to_owned()
        } else {
            String::new()
        },
    }
}

/// A branch type and slug, in the Python tool's precedence: explicit
/// `--type`/`--slug`, then the naming model's answer (`named`, always `None`
/// until the LLM milestone), then `fallback_type` and the mechanical slug.
#[must_use]
pub fn choose_name(
    text: &str,
    flags: (Option<&str>, Option<&str>),
    named: Option<(&str, &str)>,
    fallback_type: &str,
    stopwords: &[String],
) -> (String, String) {
    let (flag_type, flag_slug) = flags;
    let typ = flag_type
        .or_else(|| named.map(|n| n.0))
        .unwrap_or(fallback_type)
        .to_owned();
    let slug = flag_slug
        .or_else(|| named.map(|n| n.1))
        .map_or_else(|| slugify(text, 4, stopwords), str::to_owned);
    (typ, slug)
}

/// `wt item`'s refusal of a mechanical name that is about to be pushed and
/// written into the work item's link: it must not depend on whether the
/// model was reachable this minute. `--no-llm` accepts it.
#[must_use]
pub fn item_name_refusal(chosen: &ChosenName, push: bool, no_llm: bool) -> Option<AppError> {
    (!chosen.fallback_reason.is_empty() && push && !no_llm).then(|| AppError::NamingFellBack {
        typ: chosen.typ.clone(),
        slug: chosen.slug.clone(),
        reason: chosen.fallback_reason.clone(),
    })
}

/// The worktree's name for a hook payload, in the Python tool's precedence:
/// `payload.name`, then `--name`, then a `claude-` name built from the
/// session id's first 8 characters, or, lacking one, `pid` - injected rather
/// than read here, so a re-run with the same session id keeps landing on the
/// same name without this needing to touch the process itself.
#[must_use]
pub fn hook_name(
    payload: &serde_json::Map<String, serde_json::Value>,
    cli_name: Option<&str>,
    pid: u32,
) -> String {
    let payload_name = payload
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty());
    if let Some(name) = payload_name.or(cli_name).filter(|s| !s.is_empty()) {
        return name.to_owned();
    }
    let session = payload
        .get("session_id")
        .or_else(|| payload.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let prefix: String = session.chars().take(8).collect();
    if prefix.is_empty() {
        format!("claude-{pid}")
    } else {
        format!("claude-{prefix}")
    }
}

/// The branch a hook-create names: `name` verbatim when it is already a full
/// branch spec (the CLI validates a caller-supplied ref the same way, and the
/// hook takes the same kind of input from stdin), else `{hook_type or
/// "feat"}/{slugify(name, 6 words)}`.
#[must_use]
pub fn hook_branch_name(name: &str, hook_type: Option<&str>, stopwords: &[String]) -> String {
    if is_branch_spec(name) {
        name.to_owned()
    } else {
        format!(
            "{}/{}",
            hook_type.unwrap_or("feat"),
            slugify(name, 6, stopwords)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------- choose_name

    #[test]
    fn flags_beat_the_model_which_beats_the_fallback() {
        let text = "Rydde opp i gamle feilmeldinger";
        assert_eq!(
            choose_name(text, (None, None), None, "feat", &[]),
            (
                "feat".to_owned(),
                "rydde-opp-gamle-feilmeldinger".to_owned()
            )
        );
        assert_eq!(
            choose_name(
                text,
                (None, None),
                Some(("chore", "old-errors")),
                "feat",
                &[]
            ),
            ("chore".to_owned(), "old-errors".to_owned())
        );
        assert_eq!(
            choose_name(
                text,
                (Some("fix"), Some("x")),
                Some(("chore", "old-errors")),
                "feat",
                &[]
            ),
            ("fix".to_owned(), "x".to_owned())
        );
        assert_eq!(
            choose_name(text, (Some("docs"), None), None, "feat", &[]),
            (
                "docs".to_owned(),
                "rydde-opp-gamle-feilmeldinger".to_owned()
            )
        );
    }

    fn slug(text: &str) -> String {
        slugify(text, 4, &[])
    }

    // --------------------------------------------------------------- TestSlugify

    #[test]
    fn test_folds_norwegian_letters() {
        assert_eq!(slug("Går på økt"), "gar-okt");
        assert_eq!(slug("Blåbær"), "blabaer");
    }

    #[test]
    fn test_drops_stopwords() {
        assert_eq!(
            slug("Innlogging går i uendelig redirect-loop"),
            "innlogging-gar-uendelig-redirect"
        );
    }

    #[test]
    fn test_all_stopwords_falls_back_to_raw_words() {
        assert_eq!(slug("the and of"), "the-and-of");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(slug("!!!"), "work");
    }

    #[test]
    fn configured_stopwords_replace_the_built_in_list() {
        let own = vec!["loop".to_owned()];
        assert_eq!(slugify("the redirect loop", 4, &own), "the-redirect");
    }

    // ------------------------------------------------------------ TestBranchSpec

    #[test]
    fn test_accepts_full_branch_name() {
        assert!(is_branch_spec("feat/xledger-error-cleanup"));
    }

    #[test]
    fn test_accepts_nested() {
        assert!(is_branch_spec("feat/area/sub-thing"));
    }

    #[test]
    fn test_accepts_prefix_outside_types() {
        for good in ["wip/foo", "infra/team-access"] {
            assert!(is_branch_spec(good), "{good}");
        }
    }

    #[test]
    fn test_rejects_prose_and_uppercase() {
        for bad in [
            "rydde opp i gamle feilmeldinger",
            "Feat/X",
            "feat/",
            "feat",
            "feat-x/y",
            "feat//x",
            "feat/x-",
            "/x",
        ] {
            assert!(!is_branch_spec(bad), "{bad}");
        }
    }

    // ------------------------------------------------------------------------ F15

    #[test]
    fn f15_slug_shape() {
        for good in ["x", "msal-login-loop", "a1-b2"] {
            assert!(is_slug(good), "{good}");
        }
        for bad in ["", "-x", "x-", "a--b", "Upper", "a_b", "a/b"] {
            assert!(!is_slug(bad), "{bad}");
        }
    }

    // --------------------------------------------------------- TestFill / Strict

    #[test]
    fn test_substitutes() {
        let out = fill(
            "{type}/{id}-{slug}",
            &[("type", "fix"), ("id", "21438"), ("slug", "x")],
        )
        .unwrap();
        assert_eq!(out, "fix/21438-x");
    }

    #[test]
    fn test_unresolved_placeholder_raises() {
        let err = fill("../{rep}.worktrees", &[("repo", "myrepo")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "unresolved placeholder in template '../{rep}.worktrees' -> '../{rep}.worktrees'"
        );
    }

    #[test]
    fn test_all_resolved_is_fine() {
        assert_eq!(
            fill("../{repo}.worktrees", &[("repo", "myrepo")]).unwrap(),
            "../myrepo.worktrees"
        );
    }

    #[test]
    fn f11_other_braces_are_literal() {
        let script = "if ($x) { echo {path} } else {Exit 1} {}";
        assert_eq!(
            fill(script, &[("path", "/t")]).unwrap(),
            "if ($x) { echo /t } else {Exit 1} {}"
        );
    }

    #[test]
    fn f11_a_substituted_value_is_not_rescanned() {
        assert_eq!(fill("{slug}", &[("slug", "{x}")]).unwrap(), "{x}");
    }

    // ---------------------------------------------------------- claude_argv

    #[test]
    fn test_claude_argv_shape() {
        let argv = claude_argv("/usr/bin/claude", "Fix the login bug", "haiku", "SYS");
        assert_eq!(
            argv,
            vec![
                "/usr/bin/claude",
                "-p",
                "Work item title: Fix the login bug",
                "--model",
                "haiku",
                "--output-format",
                "text",
                "--system-prompt",
                "SYS",
            ]
        );
    }

    // ------------------------------------------------------- parse_model_reply

    #[test]
    fn test_parses_a_fenced_reply() {
        let reply = "```json\n{\"type\":\"fix\",\"slug\":\"login-redirect-loop\"}\n```";
        assert_eq!(
            parse_model_reply(reply).unwrap(),
            ("fix".to_owned(), "login-redirect-loop".to_owned())
        );
    }

    #[test]
    fn test_parses_json_with_prose_around_it() {
        let reply =
            "Sure, here you go:\n{\"type\":\"FEAT\",\"slug\":\"Add-Export\"}\nHope that helps!";
        assert_eq!(
            parse_model_reply(reply).unwrap(),
            ("feat".to_owned(), "add-export".to_owned())
        );
    }

    #[test]
    fn test_no_json_at_all_is_named() {
        assert_eq!(
            parse_model_reply("no braces here").unwrap_err(),
            "the naming model returned no JSON"
        );
    }

    #[test]
    fn test_malformed_json_is_named() {
        assert_eq!(
            parse_model_reply("{not: json}").unwrap_err(),
            "the naming model returned malformed JSON"
        );
    }

    #[test]
    fn test_a_bad_type_is_named() {
        assert_eq!(
            parse_model_reply(r#"{"type":"feature","slug":"a-b"}"#).unwrap_err(),
            "the naming model proposed an unusable name (feature/a-b)"
        );
    }

    #[test]
    fn test_a_too_short_slug_is_named() {
        assert_eq!(
            parse_model_reply(r#"{"type":"feat","slug":"onlyone"}"#).unwrap_err(),
            "the naming model proposed an unusable name (feat/onlyone)"
        );
    }

    #[test]
    fn test_a_too_long_slug_is_named() {
        let err = parse_model_reply(r#"{"type":"feat","slug":"a-b-c-d-e-f-g"}"#).unwrap_err();
        assert!(err.contains("unusable name"), "{err}");
    }

    // ------------------------------------------------------ TestLlmNameReasons

    #[test]
    fn test_disabled_naming_says_so() {
        let reason = model_skip_reason(false, "noe", true).unwrap();
        assert!(reason.contains("not requested"), "{reason}");
    }

    #[test]
    fn test_a_missing_claude_binary_is_named() {
        let reason = model_skip_reason(true, "noe", false).unwrap();
        assert!(reason.contains("PATH"), "{reason}");
    }

    #[test]
    fn test_an_empty_title_is_also_not_requested() {
        let reason = model_skip_reason(true, "", true).unwrap();
        assert!(reason.contains("not requested"), "{reason}");
    }

    // -------------------------------------------------------------- chosen_name

    #[test]
    fn fallback_reason_only_surfaces_when_the_model_was_wanted_and_declined() {
        let timeout_reason = "the naming model timed out after 60s";

        let chosen = chosen_name(
            "some prose",
            (None, None),
            None,
            timeout_reason,
            true,
            "feat",
            &[],
        );
        assert_eq!(chosen.fallback_reason, timeout_reason);

        // naming.llm off (folded into model_wanted=false): the message is
        // suppressed even though the model was asked and said nothing.
        let chosen = chosen_name(
            "some prose",
            (None, None),
            None,
            timeout_reason,
            false,
            "feat",
            &[],
        );
        assert_eq!(chosen.fallback_reason, "");

        // the model answered: no fallback at all, even carrying a non-empty
        // `why` (never happens for a real caller, but isolates the
        // `named.is_none()` half of the suppression from the `why` value
        // itself, so a mutation that drops that check is still caught).
        let chosen = chosen_name(
            "some prose",
            (None, None),
            Some(("fix", "old-errors")),
            timeout_reason,
            true,
            "feat",
            &[],
        );
        assert_eq!(chosen.fallback_reason, "");
        assert_eq!(chosen.typ, "fix");

        // --type/--slug given: the model was never wanted, same isolation.
        let chosen = chosen_name(
            "some prose",
            (Some("chore"), Some("x")),
            None,
            timeout_reason,
            false,
            "feat",
            &[],
        );
        assert_eq!(chosen.fallback_reason, "");
        assert_eq!(chosen.typ, "chore");
    }

    // ------------------------------------------------------------- hook_name

    fn obj(pairs: &[(&str, &str)]) -> serde_json::Map<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), serde_json::Value::String((*v).to_owned())))
            .collect()
    }

    #[test]
    fn payload_name_wins_over_everything() {
        let payload = obj(&[("name", "feat/x"), ("session_id", "aaaaaaaa-1")]);
        assert_eq!(hook_name(&payload, Some("--name-val"), 1), "feat/x");
    }

    #[test]
    fn cli_name_is_the_fallback_when_the_payload_has_none() {
        let payload = obj(&[("session_id", "aaaaaaaa-1")]);
        assert_eq!(hook_name(&payload, Some("cli-name"), 1), "cli-name");
    }

    #[test]
    fn two_sessions_get_different_names_from_their_first_8_chars() {
        let a = obj(&[("session_id", "aaaaaaaa-1111")]);
        let b = obj(&[("session_id", "bbbbbbbb-2222")]);
        assert_ne!(hook_name(&a, None, 1), hook_name(&b, None, 1));
    }

    #[test]
    fn the_same_session_id_names_the_same_worktree_twice() {
        let payload = obj(&[("session_id", "aaaaaaaa-1111")]);
        assert_eq!(
            hook_name(&payload, None, 111),
            hook_name(&payload, None, 222)
        );
    }

    #[test]
    fn camel_case_session_id_is_also_read() {
        let payload = obj(&[("sessionId", "aaaaaaaa-1111")]);
        assert_eq!(hook_name(&payload, None, 1), "claude-aaaaaaaa");
    }

    #[test]
    fn the_pid_is_the_last_resort() {
        assert_eq!(
            hook_name(&serde_json::Map::new(), None, 4242),
            "claude-4242"
        );
    }

    // ------------------------------------------------------- hook_branch_name

    #[test]
    fn a_full_branch_name_is_taken_verbatim() {
        assert_eq!(
            hook_branch_name("feat/xledger-error-cleanup", Some("chore"), &[]),
            "feat/xledger-error-cleanup"
        );
    }

    #[test]
    fn prose_is_slugified_under_the_hook_type_or_feat() {
        assert_eq!(
            hook_branch_name("rydde opp i gamle feilmeldinger", None, &[]),
            "feat/rydde-opp-gamle-feilmeldinger"
        );
        assert_eq!(
            hook_branch_name("rydde opp i gamle feilmeldinger", Some("chore"), &[]),
            "chore/rydde-opp-gamle-feilmeldinger"
        );
    }

    #[test]
    fn an_item_name_that_fell_back_is_refused_when_it_would_be_pushed() {
        let fell_back = ChosenName {
            typ: "fix".to_owned(),
            slug: "msal-login-loop".to_owned(),
            fallback_reason: "claude is not on PATH".to_owned(),
        };
        assert_eq!(
            item_name_refusal(&fell_back, true, false)
                .unwrap()
                .to_string(),
            "naming fell back to the mechanical slug (fix/msal-login-loop): claude is not on PATH. \
             That name would be pushed and linked. Re-run with --no-llm to accept it, or pass --slug."
        );
        assert!(
            item_name_refusal(&fell_back, false, false).is_none(),
            "not pushed"
        );
        assert!(
            item_name_refusal(&fell_back, true, true).is_none(),
            "--no-llm accepts it"
        );
        let named = ChosenName {
            fallback_reason: String::new(),
            ..fell_back
        };
        assert!(item_name_refusal(&named, true, false).is_none());
    }
}
