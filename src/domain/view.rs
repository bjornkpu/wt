use std::collections::BTreeSet;

use clap_complete::env::EnvCompleter;
use jiff::Timestamp;

/// One `wt ls` line. Field 2 is named `ref_` because `ref` is a keyword;
/// it holds the branch name or `detached <sha8>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub leaf: String,
    pub ref_: String,
    pub mode: String,
    pub age: String,
    pub dirty: String,
}

/// `git status --porcelain`'s outcome for one worktree. F10: a failed
/// check gets its own flag rather than passing as clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatus {
    Clean,
    Dirty,
    Failed,
}

/// The abbreviated sha `ls` shows for a detached worktree, matching git's
/// own default width.
#[must_use]
pub fn short_sha(head: &str) -> String {
    head.chars().take(8).collect()
}

/// F9: `created`'s age in whole days, `""` when there is no `created` at
/// all, and `"?"` when it is present but not a timestamp `ls` can parse.
#[must_use]
pub fn age_str(created: Option<&str>, now: Timestamp) -> String {
    let Some(created) = created else {
        return String::new();
    };
    let Ok(created) = created.parse::<Timestamp>() else {
        return "?".to_owned();
    };
    let Some(secs) = now.as_second().checked_sub(created.as_second()) else {
        return "?".to_owned();
    };
    let Some(days) = secs.checked_div(86_400) else {
        return "?".to_owned();
    };
    format!("{days}d")
}

/// The `ls` flags column: `status?` (F10) or `dirty`, plus `exec-failed`
/// when the sidecar recorded a failed exec step.
#[must_use]
pub fn dirty_flags(status: GitStatus, exec_failed: bool) -> String {
    let mut flags = Vec::new();
    match status {
        GitStatus::Failed => flags.push("status?"),
        GitStatus::Dirty => flags.push("dirty"),
        GitStatus::Clean => {}
    }
    if exec_failed {
        flags.push("exec-failed");
    }
    flags.join(" ")
}

/// Renders `ls`'s rows: each column `ljust`ed to its widest value across
/// every row, columns joined by two spaces, then the line right-trimmed.
/// No rows (a bare repo) renders nothing.
#[must_use]
pub fn render_ls(rows: &[Row]) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }
    let mut widths = [0usize; 5];
    for r in rows {
        for (w, c) in widths.iter_mut().zip(row_cols(r)) {
            *w = (*w).max(c.chars().count());
        }
    }
    rows.iter()
        .map(|r| {
            row_cols(r)
                .iter()
                .zip(widths)
                .map(|(c, w)| pad(c, w))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned()
        })
        .collect()
}

const fn row_cols(r: &Row) -> [&str; 5] {
    [
        r.leaf.as_str(),
        r.ref_.as_str(),
        r.mode.as_str(),
        r.age.as_str(),
        r.dirty.as_str(),
    ]
}

fn pad(s: &str, width: usize) -> String {
    let len = s.chars().count();
    format!("{s}{}", " ".repeat(width.saturating_sub(len)))
}

/// `complete`'s names: every leaf and branch name, deduplicated and sorted.
#[must_use]
pub fn complete_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    names
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// What `rm --dry-run` prints. Its own function rather than a flag on the
/// removal: previewing a teardown should not be one boolean away from
/// performing it. A tree with no sidecar shows `(no metadata)` (F21).
#[must_use]
pub fn describe_removal(r: &crate::domain::plan::Removal) -> Vec<String> {
    let mode = match (&r.sidecar, &r.mode) {
        (None, _) => "(no metadata)",
        (Some(_), None) => "(unknown)",
        (Some(_), Some(mode)) => mode,
    };
    let purge = if r.purge.is_empty() {
        "(nothing)".to_owned()
    } else {
        r.purge.join(", ")
    };
    let mut lines = vec![
        format!("would remove: {}", r.path.display()),
        format!("  mode:   {mode}"),
        format!(
            "  branch: {} -> {}",
            r.branch.as_deref().unwrap_or("(detached)"),
            r.teardown.delete_branch
        ),
        format!("  purge:  {purge}"),
    ];
    if !r.reasons.is_empty() {
        lines.push(format!("  NOT CLEAN: {}", r.reasons.join(", ")));
    }
    lines
}

/// The `wt init pwsh` wrapper: a `wt` function that calls `__EXE__` (never
/// through `PATH`, so Windows Terminal's own `wt` alias cannot win),
/// followed by the completion registration `clap_complete`'s dynamic engine
/// prints for `COMPLETE=powershell __EXE__`.
const PWSH_TEMPLATE: &str = r"function wt {
    $exe = __EXE__
    # wt writes UTF-8: decode it as such, or a non-ASCII path comes back mangled.
    $encoding = [Console]::OutputEncoding
    [Console]::OutputEncoding = [Text.Encoding]::UTF8
    try {
        $verb = $null
        foreach ($a in $args) {
            if ($a -notlike '-*') { $verb = $a; break }
        }
        if ($verb -eq 'rm') {
            $main = & $exe cd
            if ($LASTEXITCODE -eq 0 -and $main -and (Get-Location).Path -ne $main) {
                Set-Location -LiteralPath $main
            }
        }
        $out = & $exe @args
        $code = $LASTEXITCODE
        if ($out) { $out | Write-Output }
        $last = $out | Select-Object -Last 1
        if ($last -and [IO.Path]::IsPathRooted($last) -and (Test-Path -LiteralPath $last -PathType Container)) {
            $target = $last
            if ('--open' -in @($args)) {
                $mainRoot = & $exe cd
                if ($LASTEXITCODE -eq 0 -and $mainRoot) { $target = $mainRoot }
            }
            Set-Location -LiteralPath $target
        }
        $global:LASTEXITCODE = $code
    } finally {
        [Console]::OutputEncoding = $encoding
    }
}
__REGISTRATION__";

/// `wt init zsh`'s wrapper, same protocol as the pwsh one (spec section 4).
const ZSH_TEMPLATE: &str = r#"wt() {
    local exe=__EXE__
    local verb="" a
    for a in "$@"; do
        [[ "$a" != -* ]] && { verb="$a"; break; }
    done
    if [[ "$verb" == "rm" ]]; then
        local main
        main=$("$exe" cd)
        if [[ $? -eq 0 && -n "$main" && "$PWD" != "$main" ]]; then
            cd -- "$main"
        fi
    fi
    local out
    out=$("$exe" "$@")
    local code=$?
    [[ -n "$out" ]] && print -r -- "$out"
    local last="${out##*$'\n'}"
    if [[ -n "$last" && "$last" == /* && -d "$last" ]]; then
        local target="$last"
        local open=0
        for a in "$@"; do [[ "$a" == "--open" ]] && open=1; done
        if (( open )); then
            local mainroot
            mainroot=$("$exe" cd)
            [[ $? -eq 0 && -n "$mainroot" ]] && target="$mainroot"
        fi
        cd -- "$target"
    fi
    return $code
}
__REGISTRATION__"#;

/// Quotes `exe` as a pwsh single-quoted literal: backslashes (Windows
/// paths) are literal inside single quotes, so only an embedded `'` needs
/// doubling.
fn pwsh_quote(exe: &str) -> String {
    format!("'{}'", exe.replace('\'', "''"))
}

/// Quotes `exe` as a POSIX single-quoted literal: an embedded `'` closes
/// the quote, escapes itself, and reopens.
fn zsh_quote(exe: &str) -> String {
    format!("'{}'", exe.replace('\'', r"'\''"))
}

/// What `clap_complete`'s dynamic engine prints for `COMPLETE={shell} exe`,
/// generated directly (no `Command` is needed: `write_registration` never
/// reads one) so `init`'s script always matches what the engine itself
/// would register.
fn registration(shell: &dyn EnvCompleter, exe: &str) -> String {
    let mut buf = Vec::new();
    // A `Vec<u8>` writer cannot fail; there is nothing for the caller to act on.
    let _ = shell.write_registration("COMPLETE", "wt", "wt", exe, &mut buf);
    String::from_utf8(buf).unwrap_or_default()
}

/// pwsh's registration, with the completer path re-quoted pwsh's way instead
/// of `clap_complete`'s. `clap_complete` quotes it POSIX/Fish-style
/// (`shlex`), which bans a bare `\` in single quotes, so any path with one
/// (every real Windows path) comes out double-quoted with `\` escaped as
/// `\\`. That is not actually broken - pwsh resolves the doubled backslashes
/// fine when it invokes the path, and an embedded `'` needs no escaping
/// inside `shlex`'s double quotes either - but it is `shlex`'s POSIX/Fish
/// escaping riding along in a pwsh script by coincidence rather than by
/// design, so this swaps in our own single-quoted form, which the reader
/// can check without reasoning about pwsh's path-normalisation behaviour.
/// Generated against a placeholder made only of characters `shlex` always
/// leaves unquoted, then substituted back with `pwsh_quote`, so this
/// touches only that one interpolation rather than re-deriving the whole
/// script by hand.
///
/// The completer also runs the native exe outside our `wt` function, so it
/// gets the wrapper's UTF-8 decode too, or an æøå candidate comes back
/// mangled.
fn pwsh_registration(exe: &str) -> String {
    const PLACEHOLDER: &str = "WT_EXE_PLACEHOLDER";
    registration(&clap_complete::env::Powershell, PLACEHOLDER)
        .replace(PLACEHOLDER, &pwsh_quote(exe))
        .replace(
            "    $results = Invoke-Expression @\"",
            "    $encoding = [Console]::OutputEncoding;\n    [Console]::OutputEncoding = [Text.Encoding]::UTF8;\n    try {\n        $results = Invoke-Expression @\"",
        )
        .replace(
            "\n\"@;\n",
            "\n\"@;\n    } finally {\n        [Console]::OutputEncoding = $encoding;\n    }\n",
        )
}

/// The full script `wt init pwsh` prints for `$PROFILE`.
#[must_use]
pub fn pwsh_init_script(exe: &str) -> String {
    PWSH_TEMPLATE
        .replace("__EXE__", &pwsh_quote(exe))
        .replace("__REGISTRATION__", &pwsh_registration(exe))
}

/// The full script `wt init zsh` prints for `.zshrc`.
#[must_use]
pub fn zsh_init_script(exe: &str) -> String {
    ZSH_TEMPLATE.replace("__EXE__", &zsh_quote(exe)).replace(
        "__REGISTRATION__",
        &registration(&clap_complete::env::Zsh, exe),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    // ---------------------------------------------------------------- age_str (F9)

    #[test]
    fn age_str_is_empty_with_no_created() {
        assert_eq!(age_str(None, ts("2026-01-10T00:00:00Z")), "");
    }

    #[test]
    fn f9_age_str_is_question_mark_for_a_malformed_created() {
        assert_eq!(age_str(Some("not-a-date"), ts("2026-01-10T00:00:00Z")), "?");
    }

    #[test]
    fn age_str_counts_whole_days() {
        let created = Some("2026-01-01T00:00:00Z");
        let now = ts("2026-01-04T00:00:00Z");
        assert_eq!(age_str(created, now), "3d");
    }

    // ----------------------------------------------------------------- dirty_flags (F10)

    #[test]
    fn f10_a_failed_status_check_is_flagged_not_clean() {
        assert_eq!(dirty_flags(GitStatus::Failed, false), "status?");
    }

    #[test]
    fn dirty_flags_combines_dirty_and_exec_failed() {
        assert_eq!(dirty_flags(GitStatus::Dirty, true), "dirty exec-failed");
    }

    #[test]
    fn dirty_flags_is_empty_when_clean() {
        assert_eq!(dirty_flags(GitStatus::Clean, false), "");
    }

    // -------------------------------------------------------------------- render_ls

    #[test]
    fn render_ls_is_empty_with_no_rows() {
        assert!(render_ls(&[]).is_empty());
    }

    #[test]
    fn render_ls_snapshot() {
        let rows = vec![
            Row {
                leaf: "(main)".to_owned(),
                ref_: "main".to_owned(),
                mode: String::new(),
                age: String::new(),
                dirty: String::new(),
            },
            Row {
                leaf: "feat/x".to_owned(),
                ref_: "feat/x".to_owned(),
                mode: "new".to_owned(),
                age: "3d".to_owned(),
                dirty: "dirty".to_owned(),
            },
            Row {
                leaf: "review/pr-7".to_owned(),
                ref_: "detached abc12345".to_owned(),
                mode: "pr".to_owned(),
                age: "1d".to_owned(),
                dirty: String::new(),
            },
        ];
        insta::assert_snapshot!(render_ls(&rows).join("\n"), @r"
        (main)       main
        feat/x       feat/x             new  3d  dirty
        review/pr-7  detached abc12345  pr   1d
        ");
    }

    // ---------------------------------------------------------------- complete_names

    #[test]
    fn complete_names_dedupes_and_sorts() {
        assert_eq!(
            complete_names(["b".to_owned(), "a".to_owned(), "a".to_owned()]),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    // ---------------------------------------------------------- describe_removal

    #[test]
    fn describe_removal_names_the_policy_and_what_blocks_it() {
        let r = crate::domain::plan::Removal {
            path: std::path::PathBuf::from("/r/repo.worktrees/feat/x"),
            sidecar: Some(std::path::PathBuf::from("/r/repo/.git/wt/feat%2Fx.json")),
            mode: Some("new".to_owned()),
            teardown: crate::domain::config::resolve_teardown(
                &crate::domain::config::defaults(),
                "new",
            ),
            reasons: vec![
                "uncommitted changes".to_owned(),
                "unpushed commits".to_owned(),
            ],
            branch: Some("feat/x".to_owned()),
            purge: vec![".env".to_owned(), ".databricks".to_owned()],
        };
        insta::assert_snapshot!(describe_removal(&r).join("\n"), @r"
        would remove: /r/repo.worktrees/feat/x
          mode:   new
          branch: feat/x -> if_merged
          purge:  .env, .databricks
          NOT CLEAN: uncommitted changes, unpushed commits
        ");
        let bare = crate::domain::plan::Removal {
            sidecar: None,
            mode: None,
            reasons: vec![],
            branch: None,
            purge: vec![],
            ..r
        };
        insta::assert_snapshot!(describe_removal(&bare).join("\n"), @r"
        would remove: /r/repo.worktrees/feat/x
          mode:   (no metadata)
          branch: (detached) -> if_merged
          purge:  (nothing)
        ");
    }

    // -------------------------------------------------------------- init scripts

    const FAKE_EXE: &str = "/home/bk/.cargo/bin/wt";

    #[test]
    fn pwsh_quote_doubles_an_embedded_single_quote() {
        assert_eq!(pwsh_quote(r"C:\wt's\wt.exe"), r"'C:\wt''s\wt.exe'");
    }

    #[test]
    fn zsh_quote_escapes_an_embedded_single_quote() {
        assert_eq!(zsh_quote("/home/o'brien/wt"), r"'/home/o'\''brien/wt'");
    }

    #[test]
    fn pwsh_init_script_snapshot() {
        insta::assert_snapshot!(pwsh_init_script(FAKE_EXE), @r#"
        function wt {
            $exe = '/home/bk/.cargo/bin/wt'
            # wt writes UTF-8: decode it as such, or a non-ASCII path comes back mangled.
            $encoding = [Console]::OutputEncoding
            [Console]::OutputEncoding = [Text.Encoding]::UTF8
            try {
                $verb = $null
                foreach ($a in $args) {
                    if ($a -notlike '-*') { $verb = $a; break }
                }
                if ($verb -eq 'rm') {
                    $main = & $exe cd
                    if ($LASTEXITCODE -eq 0 -and $main -and (Get-Location).Path -ne $main) {
                        Set-Location -LiteralPath $main
                    }
                }
                $out = & $exe @args
                $code = $LASTEXITCODE
                if ($out) { $out | Write-Output }
                $last = $out | Select-Object -Last 1
                if ($last -and [IO.Path]::IsPathRooted($last) -and (Test-Path -LiteralPath $last -PathType Container)) {
                    $target = $last
                    if ('--open' -in @($args)) {
                        $mainRoot = & $exe cd
                        if ($LASTEXITCODE -eq 0 -and $mainRoot) { $target = $mainRoot }
                    }
                    Set-Location -LiteralPath $target
                }
                $global:LASTEXITCODE = $code
            } finally {
                [Console]::OutputEncoding = $encoding
            }
        }

        Register-ArgumentCompleter -Native -CommandName wt -ScriptBlock {
            param($wordToComplete, $commandAst, $cursorPosition)

            $prev = $env:COMPLETE;
            $env:COMPLETE = "powershell";

            $args = $commandAst.Extent.Text
            $args = $args.Substring(0, [math]::Min($cursorPosition, $args.Length));
            if ($wordToComplete -eq "") {
                $args += " ''";
            }

            $encoding = [Console]::OutputEncoding;
            [Console]::OutputEncoding = [Text.Encoding]::UTF8;
            try {
                $results = Invoke-Expression @"
        & '/home/bk/.cargo/bin/wt' -- $args
        "@;
            } finally {
                [Console]::OutputEncoding = $encoding;
            }
            if ($null -eq $prev) {
                Remove-Item Env:\COMPLETE;
            } else {
                $env:COMPLETE = $prev;
            }
            $results | ForEach-Object {
                $split = $_.Split("`t");
                $cmd = $split[0];

                if ($split.Length -eq 2) {
                    $help = $split[1];
                }
                else {
                    $help = $split[0];
                }

                [System.Management.Automation.CompletionResult]::new($cmd, $cmd, 'ParameterValue', $help)
            }
        };
        "#);
    }

    #[test]
    fn zsh_init_script_snapshot() {
        insta::assert_snapshot!(zsh_init_script(FAKE_EXE), @r#"
        wt() {
            local exe='/home/bk/.cargo/bin/wt'
            local verb="" a
            for a in "$@"; do
                [[ "$a" != -* ]] && { verb="$a"; break; }
            done
            if [[ "$verb" == "rm" ]]; then
                local main
                main=$("$exe" cd)
                if [[ $? -eq 0 && -n "$main" && "$PWD" != "$main" ]]; then
                    cd -- "$main"
                fi
            fi
            local out
            out=$("$exe" "$@")
            local code=$?
            [[ -n "$out" ]] && print -r -- "$out"
            local last="${out##*$'\n'}"
            if [[ -n "$last" && "$last" == /* && -d "$last" ]]; then
                local target="$last"
                local open=0
                for a in "$@"; do [[ "$a" == "--open" ]] && open=1; done
                if (( open )); then
                    local mainroot
                    mainroot=$("$exe" cd)
                    [[ $? -eq 0 && -n "$mainroot" ]] && target="$mainroot"
                fi
                cd -- "$target"
            fi
            return $code
        }
        #compdef wt
        function _clap_dynamic_completer_wt() {
            local _CLAP_COMPLETE_INDEX=$(expr $CURRENT - 1)
            local _CLAP_IFS=$'\n'

            local completions=("${(@f)$( \
                _CLAP_IFS="$_CLAP_IFS" \
                _CLAP_COMPLETE_INDEX="$_CLAP_COMPLETE_INDEX" \
                COMPLETE="zsh" \
                /home/bk/.cargo/bin/wt -- "${words[@]}" 2>/dev/null \
            )}")

            if [[ -n $completions ]]; then
                local -a dirs=()
                local -a other=()
                local completion
                for completion in $completions; do
                    local value="${completion%%:*}"
                    if [[ "$value" == */ ]]; then
                        local dir_no_slash="${value%/}"
                        if [[ "$completion" == *:* ]]; then
                            local desc="${completion#*:}"
                            dirs+=("$dir_no_slash:$desc")
                        else
                            dirs+=("$dir_no_slash")
                        fi
                    else
                        other+=("$completion")
                    fi
                done
                [[ -n $dirs ]] && _describe -V 'values' dirs -S '/' -r '/'
                [[ -n $other ]] && _describe -V 'values' other
            fi
        }

        compdef _clap_dynamic_completer_wt wt
        "#);
    }

    #[test]
    fn pwsh_init_script_snapshot_with_a_space_in_the_exe_path() {
        // A Windows install path (`C:\Program Files\...`): locks in that both
        // our own `$exe` line and `pwsh_registration`'s completer path use a
        // single-quoted literal, the space kept literal inside it.
        insta::assert_snapshot!(pwsh_init_script(r"C:\Program Files\wt\wt.exe"), @r#"
        function wt {
            $exe = 'C:\Program Files\wt\wt.exe'
            # wt writes UTF-8: decode it as such, or a non-ASCII path comes back mangled.
            $encoding = [Console]::OutputEncoding
            [Console]::OutputEncoding = [Text.Encoding]::UTF8
            try {
                $verb = $null
                foreach ($a in $args) {
                    if ($a -notlike '-*') { $verb = $a; break }
                }
                if ($verb -eq 'rm') {
                    $main = & $exe cd
                    if ($LASTEXITCODE -eq 0 -and $main -and (Get-Location).Path -ne $main) {
                        Set-Location -LiteralPath $main
                    }
                }
                $out = & $exe @args
                $code = $LASTEXITCODE
                if ($out) { $out | Write-Output }
                $last = $out | Select-Object -Last 1
                if ($last -and [IO.Path]::IsPathRooted($last) -and (Test-Path -LiteralPath $last -PathType Container)) {
                    $target = $last
                    if ('--open' -in @($args)) {
                        $mainRoot = & $exe cd
                        if ($LASTEXITCODE -eq 0 -and $mainRoot) { $target = $mainRoot }
                    }
                    Set-Location -LiteralPath $target
                }
                $global:LASTEXITCODE = $code
            } finally {
                [Console]::OutputEncoding = $encoding
            }
        }

        Register-ArgumentCompleter -Native -CommandName wt -ScriptBlock {
            param($wordToComplete, $commandAst, $cursorPosition)

            $prev = $env:COMPLETE;
            $env:COMPLETE = "powershell";

            $args = $commandAst.Extent.Text
            $args = $args.Substring(0, [math]::Min($cursorPosition, $args.Length));
            if ($wordToComplete -eq "") {
                $args += " ''";
            }

            $encoding = [Console]::OutputEncoding;
            [Console]::OutputEncoding = [Text.Encoding]::UTF8;
            try {
                $results = Invoke-Expression @"
        & 'C:\Program Files\wt\wt.exe' -- $args
        "@;
            } finally {
                [Console]::OutputEncoding = $encoding;
            }
            if ($null -eq $prev) {
                Remove-Item Env:\COMPLETE;
            } else {
                $env:COMPLETE = $prev;
            }
            $results | ForEach-Object {
                $split = $_.Split("`t");
                $cmd = $split[0];

                if ($split.Length -eq 2) {
                    $help = $split[1];
                }
                else {
                    $help = $split[0];
                }

                [System.Management.Automation.CompletionResult]::new($cmd, $cmd, 'ParameterValue', $help)
            }
        };
        "#);
    }

    #[test]
    fn pwsh_completer_decodes_as_utf8_like_the_wrapper() {
        // Fails loudly if a `clap_complete` upgrade moves the anchors
        // `pwsh_registration` patches.
        let script = pwsh_init_script(FAKE_EXE);
        assert_eq!(
            script
                .matches("[Console]::OutputEncoding = [Text.Encoding]::UTF8")
                .count(),
            2,
            "{script}"
        );
    }

    #[test]
    fn pwsh_registration_uses_our_own_quoting_for_an_embedded_single_quote() {
        // `clap_complete`'s own quoting (`shlex`) would render this path as
        // `"C:\\Users\\o'brien\\wt.exe"` (double-quoted, `\` doubled, the `'`
        // left bare - valid pwsh, not the POSIX `'\''` escape). Our own
        // single-quoted form is still used instead, for the legibility this
        // module's other quoting already has, not because that form breaks.
        let script = pwsh_init_script(r"C:\Users\o'brien\wt.exe");
        assert!(
            script.contains(r"& 'C:\Users\o''brien\wt.exe' -- $args"),
            "{script}"
        );
    }
}
