mod domain;
mod error;
mod io;

use std::ffi::OsStr;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use clap::builder::PossibleValuesParser;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::CompleteEnv;
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};

use crate::error::AppError;

/// Turns an intent into a git worktree: a sibling directory, seeded and tracked.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Suppress non-essential chatter; failures still print. Accepted
    /// anywhere on the line (F14).
    #[arg(short = 'q', long, global = true)]
    quiet: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// New work, no tracker id.
    New {
        /// A full branch name ("feat/xledger-error-cleanup") is used
        /// verbatim; anything else is slugified into one.
        #[arg(value_name = "<type>/<slug> | description")]
        spec: String,
        /// The branch type for a description (F15: a conventional type).
        #[arg(long = "type", value_parser = PossibleValuesParser::new(domain::naming::TYPES))]
        typ: Option<String>,
        /// The branch slug for a description (F15: lowercase kebab-case).
        #[arg(long, value_parser = parse_slug)]
        slug: Option<String>,
        /// Skip the naming model.
        #[arg(long)]
        no_llm: bool,
        /// Open as a herdr workspace; the exec steps are left to it.
        #[arg(long)]
        open: bool,
        /// Run CMD in the new workspace after its exec steps (needs --open).
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
    },
    /// A tree for a tracker item: named from its title, pushed, linked and
    /// moved to the configured state.
    Item {
        /// The work item (ADO) or issue (GitHub) number.
        id: String,
        /// The branch type (F15: a conventional type).
        #[arg(long = "type", value_parser = PossibleValuesParser::new(domain::naming::TYPES))]
        typ: Option<String>,
        /// The branch slug (F15: lowercase kebab-case).
        #[arg(long, value_parser = parse_slug)]
        slug: Option<String>,
        /// Skip the naming model, and accept the mechanical name it leaves.
        #[arg(long)]
        no_llm: bool,
        /// Open as a herdr workspace; the exec steps are left to it.
        #[arg(long)]
        open: bool,
        /// Run CMD in the new workspace after its exec steps (needs --open).
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
    },
    /// A detached review tree at a PR's head.
    Pr {
        /// The PR number.
        id: String,
        /// Open as a herdr workspace; the exec steps are left to it.
        #[arg(long)]
        open: bool,
        /// Run CMD in the new workspace after its exec steps (needs --open).
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
    },
    /// A review tree at a remote branch: detached, or tracking with --track.
    Branch {
        /// The remote branch, without the remote.
        name: String,
        /// A real tracking branch instead of a detached tree.
        #[arg(long)]
        track: bool,
        /// Open as a herdr workspace; the exec steps are left to it.
        #[arg(long)]
        open: bool,
        /// Run CMD in the new workspace after its exec steps (needs --open).
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
    },
    /// List worktrees, with mode, age and dirty flags.
    Ls,
    /// Print an existing worktree's path (the shell wrapper does the cd).
    Cd {
        /// A branch name, a leaf directory, or an unambiguous suffix. The
        /// main worktree when omitted.
        #[arg(add = ArgValueCompleter::new(complete_candidates))]
        name: Option<String>,
    },
    /// Reopen an existing worktree as a herdr workspace.
    Open {
        /// A branch name, a leaf directory, or an unambiguous suffix.
        #[arg(add = ArgValueCompleter::new(complete_candidates))]
        name: String,
        /// Run CMD in the workspace.
        #[arg(long, value_name = "CMD")]
        run: Option<String>,
    },
    /// Remove a worktree under the teardown policy its sidecar recorded.
    Rm {
        /// A branch name, a leaf directory, or an unambiguous suffix.
        #[arg(
            conflicts_with_all = ["stale", "merged"],
            add = ArgValueCompleter::new(complete_candidates)
        )]
        name: Option<String>,
        /// Remove every worktree past its mode's `ttl_days` (F20: conflicts
        /// with --merged and with a name).
        #[arg(long, conflicts_with_all = ["merged", "name"])]
        stale: bool,
        /// Remove every worktree whose branch has landed on the default
        /// branch (F20: conflicts with --stale and with a name).
        #[arg(long, conflicts_with_all = ["stale", "name"])]
        merged: bool,
        /// Remove a dirty tree, or one with no wt metadata.
        #[arg(long)]
        force: bool,
        /// Show what would be removed, and why it might be refused.
        #[arg(long)]
        dry_run: bool,
    },
    /// Names for shell tab completion, one per line.
    #[command(hide = true)]
    Complete,
    /// Claude Code's `WorktreeCreate` hook: reads a JSON payload from stdin
    /// and quietly creates (or reuses) that session's worktree. Hidden: it
    /// must never appear in shell completion.
    #[command(hide = true)]
    HookCreate {
        /// The name to use when the payload carries none.
        #[arg(long)]
        name: Option<String>,
    },
    /// Claude Code's `WorktreeRemove` hook: reads a JSON payload from stdin
    /// and removes the worktree at its path. Hidden: it must never appear in
    /// shell completion.
    #[command(hide = true)]
    HookRemove,
    /// Print the `$PROFILE`/`.zshrc` wrapper and completion registration.
    #[command(hide = true)]
    Init { shell: InitShell },
}

/// The shells `wt init` prints a wrapper for.
#[derive(Clone, Copy, ValueEnum)]
enum InitShell {
    Pwsh,
    Zsh,
}

fn parse_slug(s: &str) -> Result<String, String> {
    if domain::naming::is_slug(s) {
        Ok(s.to_owned())
    } else {
        Err("must be lowercase kebab-case: [a-z0-9]+(-[a-z0-9]+)*".to_owned())
    }
}

fn main() -> ExitCode {
    // Handled before anything else touches stdout, logging, or config: a
    // `COMPLETE=<shell> wt -- ...` invocation from a registered completer
    // exits here and never reaches the rest of `main`.
    CompleteEnv::with_factory(Cli::command).complete();
    match try_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("wt: {e}");
            ExitCode::FAILURE
        }
    }
}

// One flat arm per verb: splitting the dispatch only hides it.
#[allow(clippy::too_many_lines)]
fn try_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    refuse_run_without_open(&cli.command)?;
    let home = std::env::home_dir();
    let paths =
        domain::paths::resolve(home.as_deref(), |key| std::env::var_os(key).map(Into::into))?;
    // Best-effort: a hook must not fail because the log dir cannot be made.
    if let Err(e) = init_logging(&paths.log_dir) {
        eprintln!("wt: logging is off: {e:#}");
    }
    #[cfg(windows)]
    disinherit_std_handles();

    // `init` must work outside a repo (it is what puts `wt` on the shell in
    // the first place), so it runs before `gather`, which requires one.
    if let Command::Init { shell } = &cli.command {
        return print_init_script(*shell);
    }

    let quiet_gather = matches!(cli.command, Command::Complete);
    let facts = io::run::gather(&std::env::current_dir()?, &paths.config_dir, quiet_gather)?;
    match cli.command {
        Command::New {
            spec,
            typ,
            slug,
            no_llm,
            open,
            run,
            ..
        } => {
            let args = io::run::NewArgs {
                spec: &spec,
                typ: typ.as_deref(),
                slug: slug.as_deref(),
                no_llm,
                open,
                run: run.as_deref(),
            };
            print_created(io::run::new(&facts, &args, cli.quiet)?)?;
        }
        Command::Item {
            id,
            typ,
            slug,
            no_llm,
            open,
            run,
        } => {
            let args = io::run::ItemArgs {
                id: &id,
                typ: typ.as_deref(),
                slug: slug.as_deref(),
                no_llm,
                open,
                run: run.as_deref(),
            };
            print_created(io::run::item(&facts, &args, cli.quiet)?)?;
        }
        Command::Pr { id, open, run } => {
            let args = io::run::PrArgs {
                id: &id,
                open,
                run: run.as_deref(),
            };
            print_created(io::run::pr(&facts, &args, cli.quiet)?)?;
        }
        Command::Branch {
            name,
            track,
            open,
            run,
        } => {
            let args = io::run::BranchArgs {
                name: &name,
                track,
                open,
                run: run.as_deref(),
            };
            print_created(io::run::branch(&facts, &args, cli.quiet)?)?;
        }
        Command::Ls => print_lines(&io::run::ls(&facts))?,
        Command::Open { name, run } => io::run::open(&facts, &name, run.as_deref())?,
        Command::Cd { name } => {
            let path = io::run::cd(&facts, name.as_deref())?;
            write_stdout(&format!(
                "{}
",
                path.display()
            ))?;
        }
        Command::Rm {
            name,
            stale,
            merged,
            force,
            dry_run,
        } => {
            let args = io::run::RmArgs {
                name: name.as_deref(),
                stale,
                merged,
                force,
                dry_run,
            };
            io::run::rm(&facts, &args, cli.quiet)?;
        }
        Command::Complete => print_lines(&io::run::complete(&facts))?,
        Command::HookCreate { name } => {
            let payload = read_stdin_payload()?;
            let args = io::run::HookCreateArgs {
                name: name.as_deref(),
                payload: &payload,
                pid: std::process::id(),
            };
            let created = io::run::hook_create(&facts, &args)?;
            write_stdout(&format!(
                "{}
",
                created.path.display()
            ))?;
            // A failed exec step leaves a usable tree: failing the hook would
            // make Claude Code drop it, so this one warns and exits 0.
            if let Some(e) = created.partial {
                eprintln!("wt: {e}");
            }
        }
        Command::HookRemove => io::run::hook_remove(&facts, &read_stdin_payload()?)?,
        // Unreachable: handled above, before `facts` existed to hand this
        // arm. Kept instead of `unreachable!()` (denied) so a future change
        // to that early return still does the right thing here.
        Command::Init { shell } => return print_init_script(shell),
    }
    Ok(())
}

/// `--run` on a create verb types into the workspace `--open` makes. An
/// empty `--run` is absent (Python's falsy check), so it is not refused.
const fn refuse_run_without_open(command: &Command) -> Result<(), AppError> {
    if let Command::New {
        run: Some(run),
        open: false,
        ..
    }
    | Command::Item {
        run: Some(run),
        open: false,
        ..
    }
    | Command::Pr {
        run: Some(run),
        open: false,
        ..
    }
    | Command::Branch {
        run: Some(run),
        open: false,
        ..
    } = command
        && !run.is_empty()
    {
        return Err(AppError::RunWithoutOpen);
    }
    Ok(())
}

/// The hook payload JSON, read whole from stdin: what both `hook-create` and
/// `hook-remove` take their arguments from.
fn read_stdin_payload() -> anyhow::Result<Vec<u8>> {
    let mut payload = Vec::new();
    std::io::Read::read_to_end(&mut std::io::stdin(), &mut payload)?;
    Ok(payload)
}

/// The path goes out even when a later step failed (spec section 4), so the
/// wrapper still cds; the failure then sets the exit code.
fn print_created(created: io::run::Created) -> anyhow::Result<()> {
    write_stdout(&format!(
        "{}
",
        created.path.display()
    ))?;
    created.partial.map_or(Ok(()), |e| Err(e.into()))
}

fn print_lines(lines: &[String]) -> anyhow::Result<()> {
    lines.iter().try_for_each(|line| {
        write_stdout(&format!(
            "{line}
"
        ))
    })
}

/// Writes `text` to stdout. A reader that went away (`wt cd | head -0`) is
/// not wt's failure: the text had nowhere to go, so it is dropped quietly.
fn write_stdout(text: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(e.into()),
        _ => Ok(()),
    }
}

/// `wt init pwsh|zsh`: the wrapper plus completion registration for the
/// binary this process was run from (never `PATH`, so Windows Terminal's
/// own `wt` alias cannot win).
fn print_init_script(shell: InitShell) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.display().to_string();
    let script = match shell {
        InitShell::Pwsh => domain::view::pwsh_init_script(&exe),
        InitShell::Zsh => domain::view::zsh_init_script(&exe),
    };
    write_stdout(&script)
}

/// Worktree names for `rm`, `cd` and `open`: the same list
/// `wt complete` offers. A shell's tab press must never error just because
/// the cursor sits outside a repo - there is no verb here to report it
/// against, so this fails silently to no candidates instead.
fn complete_candidates(current: &OsStr) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    complete_names_outside_ok()
        .into_iter()
        .filter(|n| n.starts_with(current))
        .map(CompletionCandidate::new)
        .collect()
}

fn complete_names_outside_ok() -> Vec<String> {
    let Ok(paths) = domain::paths::resolve(std::env::home_dir().as_deref(), |key| {
        std::env::var_os(key).map(Into::into)
    }) else {
        return Vec::new();
    };
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    let Ok(facts) = io::run::gather(&cwd, &paths.config_dir, true) else {
        return Vec::new();
    };
    io::run::complete(&facts)
}

/// Windows hands every inheritable handle to every child, so without this a
/// process an exec step leaves in the background would hold the caller's
/// stdout/stderr pipes (the shell wrapper, the `WorktreeCreate` hook) open
/// until it exits. Children still get their std handles: `Stdio` duplicates
/// an inheritable copy for each spawn. Failures are logged, not fatal.
#[cfg(windows)]
// The one FFI item in the crate: two kernel32 calls with no safe std equivalent.
#[allow(unsafe_code)]
fn disinherit_std_handles() {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn SetHandleInformation(h: *mut c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 0x1;
    // STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE: (DWORD)-10, -11, -12.
    const STD_HANDLES: [u32; 3] = [0xFFFF_FFF6, 0xFFFF_FFF5, 0xFFFF_FFF4];

    for id in STD_HANDLES {
        // SAFETY: GetStdHandle takes any DWORD, reads no memory of ours, and
        // returns a handle, null, or INVALID_HANDLE_VALUE.
        let handle = unsafe { GetStdHandle(id) };
        if handle.is_null() || handle.addr() == usize::MAX {
            continue;
        }
        // SAFETY: `handle` is this process's live std handle (checked above);
        // clearing its inherit flag changes no memory and cannot close it.
        if unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0 {
            tracing::warn!(
                "could not clear the inherit flag on std handle {id:#x}: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}

/// Appends to `wt.log` in the log dir; nothing goes to stdout.
fn init_logging(log_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(log_dir)
        .with_context(|| format!("cannot create {}", log_dir.display()))?;
    let log = tracing_appender::rolling::RollingFileAppender::builder()
        .filename_prefix("wt.log")
        .build(log_dir)?;
    tracing_subscriber::fmt()
        .with_writer(log)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .init();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::{CommandFactory, Parser};

    #[test]
    fn cli_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn quiet_is_accepted_after_the_verb() {
        // F14: -q must work anywhere on the line, not just before the verb.
        Cli::try_parse_from(["wt", "ls", "-q"]).unwrap();
        Cli::try_parse_from(["wt", "-q", "ls"]).unwrap();
        Cli::try_parse_from(["wt", "new", "feat/x", "-q"]).unwrap();
    }

    #[test]
    fn complete_candidates_is_empty_outside_a_repo_not_an_error() {
        // A shell's tab press must never crash just because the cursor is
        // outside a repo. `set_current_dir` is process-wide, but nextest
        // gives every `#[test]` its own process.
        let dir = std::env::temp_dir().join(format!("wt-complete-outside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        assert!(super::complete_candidates(std::ffi::OsStr::new("")).is_empty());
    }

    #[test]
    fn init_takes_pwsh_or_zsh_and_rejects_anything_else() {
        Cli::try_parse_from(["wt", "init", "pwsh"]).unwrap();
        Cli::try_parse_from(["wt", "init", "zsh"]).unwrap();
        assert!(Cli::try_parse_from(["wt", "init", "bash"]).is_err());
    }

    #[test]
    fn complete_hook_and_init_verbs_are_hidden() {
        let cmd = Cli::command();
        let hidden: Vec<&str> = cmd
            .get_subcommands()
            .filter(|s| s.is_hide_set())
            .map(clap::Command::get_name)
            .collect();
        assert!(hidden.contains(&"complete"), "{hidden:?}");
        assert!(hidden.contains(&"init"), "{hidden:?}");
        assert!(hidden.contains(&"hook-create"), "{hidden:?}");
        assert!(hidden.contains(&"hook-remove"), "{hidden:?}");
        for shown in ["new", "ls", "cd", "open", "rm"] {
            assert!(!hidden.contains(&shown), "{hidden:?}");
        }
    }

    #[test]
    fn rm_cd_and_open_complete_worktree_names() {
        let cmd = Cli::command();
        for verb in ["rm", "cd", "open"] {
            let sub = cmd.find_subcommand(verb).unwrap();
            let name = sub.get_arguments().find(|a| a.get_id() == "name").unwrap();
            assert!(
                name.get::<clap_complete::engine::ArgValueCompleter>()
                    .is_some(),
                "{verb}"
            );
        }
    }

    #[test]
    fn f15_type_and_slug_are_checked_by_the_parser() {
        Cli::try_parse_from(["wt", "new", "x", "--type", "fix", "--slug", "a-b"]).unwrap();
        assert!(Cli::try_parse_from(["wt", "new", "x", "--type", "feature"]).is_err());
        assert!(Cli::try_parse_from(["wt", "new", "x", "--slug", "a_b"]).is_err());
    }
}
