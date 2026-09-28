# wt

Turns an intent into a git worktree: a sibling directory, seeded and tracked. Rust port of the
Python original at `~/.config/wt`, a drop-in replacement: same config files, same sidecars,
same stdout protocol, same verbs. Worktrees the Python tool made keep working.

```
wt new feat/xledger-cleanup   verbatim branch name
wt new "msal login loop"      anything else is named by a model
wt item 21438                 work item -> branch, pushed, linked, set Active
wt pr 4521                    detached review tree
wt branch someones/work       detached unless --track
wt ls                         mode, age, dirty
wt cd [<name>]                print a worktree's path; no name means main
wt open <name>                reopen an existing tree as a herdr workspace
wt rm <name>                  teardown under the policy the sidecar recorded
wt rm --stale | --merged      reap by ttl_days, or by branches that have landed
wt init pwsh | zsh            print the shell wrapper for $PROFILE / .zshrc
```

## Verbs

| verb | flags |
|---|---|
| `new <type>/<slug>\|description` | `--type`, `--slug`, `--no-llm`, `--open`, `--run <cmd>` |
| `item <id>` | same as `new` |
| `pr <id>` | `--open`, `--run <cmd>` |
| `branch <name>` | `--track`, `--open`, `--run <cmd>` |
| `ls` | - |
| `cd [<name>]` | - |
| `open <name>` | `--run <cmd>` |
| `rm [<name>]` | `--stale`, `--merged`, `--force`, `--dry-run` |
| `init pwsh\|zsh` | - |

`-q`/`--quiet` is global: accepted anywhere on the line, on every verb. It suppresses chatter
only. Failures still print.

`--open` opens the tree as a herdr workspace, without taking focus, and hands the `exec` steps to
that workspace's shell instead of running them here. `--run <cmd>` types a command into it
afterwards; both need `--open`.

`rm`'s `<name>` matches a branch name, a leaf directory, or an unambiguous suffix. `--stale` and
`--merged` sweep instead, and conflict with a name and with each other.

`hook-create` and `hook-remove` are Claude Code's `WorktreeCreate`/`WorktreeRemove` hooks (see
[Claude Code hooks](#claude-code-hooks)).

## Install

Windows, in PowerShell:

```
irm https://github.com/bjornkpu/wt/releases/latest/download/wt-installer.ps1 | iex
```

macOS and Linux:

```
curl -LsSf https://github.com/bjornkpu/wt/releases/latest/download/wt-installer.sh | sh
```

Both drop the binary in `~/.local/bin`. Releases are GitHub Releases only, built with
cargo-dist; the crate is never published to crates.io.

`wt` is also the name of Windows Terminal's App Execution Alias
(`%LOCALAPPDATA%\Microsoft\WindowsApps`), an ordinary PATH entry. Ours wins only while
`~/.local/bin` comes before it:

    $u = [Environment]::GetEnvironmentVariable('Path','User') -split ';'; $b = "$HOME\.local\bin"; [Environment]::SetEnvironmentVariable('Path', ((@($b) + @($u | Where-Object { $_ -and $_ -ne $b })) -join ';'), 'User')

If `wt ls` ever opens a terminal instead of listing worktrees, that ordering is what broke.
Windows Terminal is still `wt.exe`.

## Shell integration

PowerShell, in `$PROFILE`:

```
Invoke-Expression (& wt init pwsh | Out-String)
```

zsh, in `.zshrc`, after `compinit` has already run:

```
eval "$(wt init zsh)"
```

Both print a `wt` function plus tab completion registration, wired to the absolute path of the
binary `init` was run from (`current_exe`) rather than through `PATH`, so Windows Terminal's own
`wt` alias can never win.

The wrapper captures the binary's stdout, re-emits it, and `cd`s there when the last line is an
existing directory. `--open` sends it to the main checkout instead of the tree it just opened.
Before `rm` it steps out to the main checkout first, because Windows refuses to remove a
directory that is a process's cwd.

## Claude Code hooks

In `~/.claude/settings.local.json`, calling the installed binary by absolute path: hooks do not
go through the shell wrapper, and a bare `wt` on PATH can resolve to Windows Terminal's alias.

```json
{
  "hooks": {
    "WorktreeCreate": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "C:/Users/<you>/.local/bin/wt.exe hook-create", "timeout": 300 }] }
    ],
    "WorktreeRemove": [
      { "matcher": "*", "hooks": [{ "type": "command", "command": "C:/Users/<you>/.local/bin/wt.exe hook-remove", "timeout": 120 }] }
    ]
  }
}
```

Wired this way, Claude cannot make or remove a worktree any other path.

## Config

`~/.config/wt/config.toml` on every OS, then `<main-root>/.wt.toml` deep-merged over it. Tables
merge; arrays and scalars replace, never append. Both files are validated on load: an unknown
key, a wrong type, or a value outside a fixed set is refused, with every problem listed at once.

Options resolve `[defaults]` → `[mode.<mode>]` → `[mode.<mode>.<provider>]` (`ado`, `github` or
`none`, detected from origin's URL). Teardown resolves `[teardown]` → `[teardown.mode.<mode>]`.

`WT_HOME`, when set and non-empty, puts config and the log under one directory
(`$WT_HOME/config.toml`, `$WT_HOME/wt.log`) instead of `~/.config/wt/config.toml` and
`~/.local/state/wt/wt.log`.

`[herdr] label_default` and `[herdr.label.<mode>]` name the herdr workspace `--open` creates,
with `{repo}`, `{id}`, `{branch}` placeholders.

`[naming]` (`llm`, `model`, `timeout`) asks a fast model, run as `claude -p`, to turn a work
item's title into a conventional type and a 2-4 word slug; it falls back to the mechanical slug
on timeout, bad output, or a missing `claude` binary. `--no-llm` skips it for one call.
`[naming.type_from_tracker]` maps the tracker's work item type to a conventional-commit type.

## Behaviour

- stdout carries only: the worktree path (`new`, `item`, `pr`, `branch`, `cd`, `hook-create`),
  the `ls` table, the `complete` names, and the `init` script. Everything else, including `exec`
  step output, goes to stderr.
- When a tree was created but a later step failed (push, link, state, exec, `--open`), the path
  is still printed and the exit code is 1. The shell wrapper still `cd`s there. One exception:
  `hook-create` exits 0 when an exec step failed (it warns on stderr), since a failing hook
  would make Claude Code drop a tree that exists.
- Redirect the output of anything you background inside an `exec` step. An abandoned pipe can
  otherwise hang the caller, including the hook path.
- A tree whose `.git` link is broken but whose directory is still there is refused outright, even
  with `--force`: delete the directory, or run `git worktree repair`. That command prints an
  error and exits 1 even when it actually fixed the tree, so check the tree afterwards rather
  than the exit code. Such a tree shows as `prunable` in `wt ls`.
- A tree whose directory was deleted by hand (nothing left to repair) shows as `missing` in
  `wt ls`, and `wt rm` cleans it up unaided. Under `delete_branch = "always"` it refuses while
  the branch holds unpushed commits; `--force` removes it anyway.

## Differences from the Python version

- No Python tracebacks. Every failure is `wt: {message}`, exit 1; a usage error from clap is
  exit 2.
- `-q` works anywhere on the line, not just before the verb.
- `--type` and `--slug` are validated by the parser instead of accepted as free strings.
- `rm --stale --merged` together is refused before any fetch, not after.
- `rm --merged` spares a branch that nothing was ever committed on.
- Org/project/repo names from the remote URL are percent-decoded before the safety check, so
  Azure DevOps projects with spaces or æøå work; legacy `*.visualstudio.com` remotes parse too.
- `pr` fetches the PR's ref before `worktree add`, so PRs from forks work.
- `wt init pwsh|zsh` replaces dot-sourcing `shell/wt.ps1`, and there is now a real zsh wrapper
  (cd and completion). Python's git-bash shim never did either.
- Completion has no cache; the Rust binary starts fast enough not to need one.

## Development

```
cargo fmt --check && cargo clippy --all-targets && cargo nextest run
```

All three must be clean before any change counts as done. See `docs/releasing.md` for cutting a
release.
