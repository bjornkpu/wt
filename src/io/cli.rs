use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use wait_timeout::ChildExt;

use crate::domain::{herdr, provider};
use crate::error::AppError;

/// How one exec step ended.
#[derive(Debug)]
pub enum ExecOutcome {
    Ok,
    /// Non-zero exit; `None` when a signal ended it.
    Failed(Option<i32>),
    TimedOut,
    Spawn(std::io::Error),
}

/// The shell exec steps run in when the config names none. pwsh is a
/// separate install Windows does not ship, so fall back to powershell
/// rather than fail after the worktree already exists.
#[must_use]
pub fn default_shell() -> Vec<String> {
    let argv: &[&str] = if !cfg!(windows) {
        &["bash", "-lc"]
    } else if on_path("pwsh") {
        &["pwsh", "-NoProfile", "-Command"]
    } else {
        &["powershell", "-NoProfile", "-Command"]
    };
    argv.iter().map(|s| (*s).to_owned()).collect()
}

fn on_path(name: &str) -> bool {
    which(name).is_some()
}

/// `name` on PATH, trying each PATHEXT extension on Windows (az is `az.cmd`).
fn which(name: &str) -> Option<PathBuf> {
    let pathext = cfg!(windows)
        .then(|| std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned()));
    which_in(name, &std::env::var_os("PATH")?, pathext.as_deref())
}

fn which_in(name: &str, path: &OsStr, pathext: Option<&str>) -> Option<PathBuf> {
    candidates(name, path, pathext)
        .into_iter()
        .find(|p| p.is_file())
}

/// Where `name` may live, in lookup order: each PATH dir, and in each one
/// every PATHEXT extension (`None`: the bare name, as off Windows).
fn candidates(name: &str, path: &OsStr, pathext: Option<&str>) -> Vec<PathBuf> {
    let exts: Vec<&str> = pathext.map_or_else(
        || vec![""],
        |p| p.split(';').filter(|e| !e.is_empty()).collect(),
    );
    std::env::split_paths(path)
        .flat_map(|dir| exts.iter().map(move |ext| dir.join(format!("{name}{ext}"))))
        .collect()
}

/// az is a Python CLI: make it speak utf-8 rather than the console codepage,
/// and keep its banners, telemetry and install prompts out of the way.
const CLI_ENV: [(&str, &str); 5] = [
    ("PYTHONIOENCODING", "utf-8"),
    ("PYTHONUTF8", "1"),
    ("AZURE_CORE_ONLY_SHOW_ERRORS", "true"),
    ("AZURE_CORE_COLLECT_TELEMETRY", "false"),
    ("AZURE_EXTENSION_USE_DYNAMIC_INSTALL", "no"),
];

const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// One finished external CLI call, its output decoded.
pub struct CliOutput {
    pub success: bool,
    /// The exit code; `None` when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs an external CLI (az, gh) in `cwd` with a null stdin, so a credential
/// or device-code prompt fails fast instead of waiting on nobody. A `.cmd`
/// goes through `cmd /c`: std does that itself for a `.bat`/`.cmd` path,
/// quoting by the real cmd rules and refusing an argument it cannot quote
/// safely. Never hand-roll it: the argv carries ids from the command line
/// and names scraped from the remote URL.
pub fn run_cli(argv: &[String], cwd: &Path, timeout: Duration) -> Result<CliOutput, AppError> {
    let Some((tool, rest)) = argv.split_first() else {
        return Err(AppError::Cli("empty command".to_owned()));
    };
    let exe = which(tool).ok_or_else(|| AppError::Cli(format!("{tool} not found on PATH")))?;
    run_exe(tool, &exe, rest, cwd, timeout)
}

/// `run_cli` once `tool` is resolved to `exe`.
fn run_exe(
    tool: &str,
    exe: &Path,
    rest: &[String],
    cwd: &Path,
    timeout: Duration,
) -> Result<CliOutput, AppError> {
    let mut child = Command::new(exe)
        .args(rest)
        .current_dir(cwd)
        .envs(CLI_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AppError::Cli(format!("cannot run {tool}: {e}")))?;
    let stdout = capture(child.stdout.take());
    let stderr = capture(child.stderr.take());
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            // ponytail: kills cmd.exe, not the python child az.cmd started,
            // which lives on until it finishes; a job object would take both.
            let _ = child.kill();
            let _ = child.wait();
            return Err(AppError::Cli(format!(
                "{tool} timed out after {}s",
                timeout.as_secs()
            )));
        }
        Err(e) => {
            let _ = child.kill();
            return Err(AppError::Cli(format!("cannot run {tool}: {e}")));
        }
    };
    // cmd /c has exited, but a process it started may still hold the pipes:
    // take what arrived within the grace, as run_exec does.
    let draining = Instant::now();
    let [stdout, stderr] = [stdout, stderr].map(|rx| {
        let left = DRAIN_GRACE.saturating_sub(draining.elapsed());
        rx.recv_timeout(left).ok().map(|b| provider::decode(&b))
    });
    let [Some(stdout), Some(stderr)] = [stdout, stderr] else {
        return Err(AppError::Cli(format!(
            "{tool} left processes holding its output"
        )));
    };
    Ok(CliOutput {
        success: status.success(),
        code: status.code(),
        stdout,
        stderr,
    })
}

/// `run_cli`'s reply as a JSON object, or Python `run_json`'s refusal.
pub fn run_json(argv: &[String], cwd: &Path) -> Result<Map<String, Value>, AppError> {
    let out = run_cli(argv, cwd, CLI_TIMEOUT)?;
    let tool = argv.first().map_or("", String::as_str);
    provider::json_object(tool, out.success, &out.stdout, &out.stderr)
}

/// An ADO GET through `az rest`, its reply read back from a UTF-8 file.
pub fn az_get(url: &str) -> Result<Map<String, Value>, AppError> {
    let out = az_rest(url, None)?;
    provider::json_object("az", out.success, &out.stdout, &out.stderr)
}

/// An ADO JSON-patch PATCH through `az rest`; `Err` is the failure's detail.
pub fn az_patch(url: &str, body: &str) -> Result<(), String> {
    match az_rest(url, Some(body)) {
        Ok(out) => provider::patch_sent(out.success, &out.stdout, &out.stderr),
        Err(e) => Err(e.to_string()),
    }
}

/// `az rest` with the retry: `stdout` is the reply file's text. Runs in the
/// temp dir and names its files relative to it, so no path (which may hold
/// æøå) crosses az's argv; the body goes in a UTF-8 file, never argv.
fn az_rest(url: &str, body: Option<&str>) -> Result<CliOutput, AppError> {
    let dir = std::env::temp_dir();
    let reply = TempFile::new(&dir, "reply");
    let body_file = body
        .map(|text| {
            let file = TempFile::new(&dir, "body");
            match std::fs::write(&file.path, text) {
                Ok(()) => Ok(file),
                Err(source) => Err(AppError::Write {
                    path: file.path.clone(),
                    source,
                }),
            }
        })
        .transpose()?;
    let argv = provider::az_rest_argv(
        url,
        &reply.name,
        body_file.as_ref().map(|f| f.name.as_str()),
    );
    let out = provider::retry(
        || run_cli(&argv, &dir, CLI_TIMEOUT),
        |out| !out.success && provider::transient(&out.stderr),
        |n| thread::sleep(provider::backoff(n, jitter())),
    )?;
    if !out.success {
        return Ok(out);
    }
    let text = std::fs::read(&reply.path).map_err(|source| AppError::Read {
        path: reply.path.clone(),
        source,
    })?;
    Ok(CliOutput {
        stdout: provider::decode(&text)
            .trim_start_matches('\u{feff}')
            .to_owned(),
        ..out
    })
}

/// `[0, 1)` from the clock: jitter for the backoff, not randomness anyone
/// relies on.
fn jitter() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    f64::from(nanos) / 1e9
}

/// A file in the temp dir, removed when dropped.
struct TempFile {
    /// Relative to the temp dir, as az is handed it.
    name: String,
    path: PathBuf,
}

impl TempFile {
    fn new(dir: &Path, tag: &str) -> Self {
        let name = format!(
            "wt-az-{}-{tag}-{}.json",
            std::process::id(),
            jitter().to_bits()
        );
        Self {
            path: dir.join(&name),
            name,
        }
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

const HERDR_TIMEOUT: Duration = Duration::from_secs(10);

/// One herdr call: its stdout on success, else `(stderr, stdout)` for the
/// failure text. A spawn error or a timeout lands in the stderr slot.
fn call_herdr(argv: &[String], cwd: &Path) -> Result<String, (String, String)> {
    match run_cli(argv, cwd, HERDR_TIMEOUT) {
        Ok(out) if out.success => Ok(out.stdout),
        Ok(out) => Err((out.stderr, out.stdout)),
        Err(e) => Err((e.to_string(), String::new())),
    }
}

/// Opens `path` as its own herdr workspace, a child of the parent
/// checkout's, and types `cmds` into its pane in order: the pane's shell
/// buffers them, so they run there while you work here. Refuses, rather
/// than letting herdr pick, when `root` has no workspace open.
pub fn herdr_open(root: &Path, path: &Path, label: &str, cmds: &[String]) -> herdr::Outcome {
    use herdr::Outcome;
    if !on_path("herdr") {
        return Outcome::Skipped(herdr::SKIPPED.to_owned());
    }
    let listed = call_herdr(&herdr::worktree_list_argv(root), root).unwrap_or_default();
    let Some(source) = herdr::parent_workspace(root, &listed) else {
        return Outcome::Failed(herdr::no_workspace(root));
    };
    let reply = match call_herdr(&herdr::worktree_open_argv(&source, path, label), root) {
        Ok(reply) => reply,
        Err((err, out)) => return Outcome::Failed(herdr::open_failed(&err, &out)),
    };
    let opened = herdr::opened(label, path);
    if cmds.is_empty() {
        return Outcome::Ok(opened);
    }
    let Some(pane) = herdr::pane_of(&reply) else {
        return Outcome::Failed(herdr::no_pane(&opened, cmds.len()));
    };
    for cmd in cmds {
        if let Err((err, out)) = call_herdr(&herdr::pane_run_argv(&pane, cmd), root) {
            return Outcome::Failed(herdr::send_failed(&opened, cmd, &err, &out));
        }
    }
    Outcome::Ok(herdr::running(&opened, cmds.len()))
}

/// Asks the herdr panes sitting in `path` to cd out to `root`: Windows will
/// not delete a live process's cwd. A hint, not a gate: herdr's pane cwd lags
/// the shell, so the removal decides, and the panes returned (not asked)
/// only explain a removal that failed. Return codes are ignored.
pub fn release_panes(root: &Path, path: &Path) -> Vec<String> {
    if !on_path("herdr") {
        return Vec::new();
    }
    let listed = call_herdr(&herdr::pane_list_argv(), root).unwrap_or_default();
    let me = std::env::var("HERDR_PANE_ID").unwrap_or_default();
    let release = herdr::release_panes(root, path, &herdr::panes(&listed), &me);
    if let Some(warning) = &release.warning {
        eprintln!("{warning}");
    }
    for argv in &release.send {
        let _ = call_herdr(argv, root);
    }
    release.left
}

/// Reads `pipe` to its end on its own thread, handing the bytes over once
/// it closes.
pub fn capture(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// How long the output readers get to finish once the step itself has
/// ended. Past it, whatever still holds the pipes is something the step left
/// running in the background, and wt stops waiting for it.
pub const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Runs `cmd` through `shell` in `cwd` with a null stdin, killed after
/// `timeout`. Its output goes through fresh pipes to wt's stderr (stdout
/// belongs to the worktree path), so a process it leaves in the background
/// holds those pipes, never the caller's.
pub fn run_exec(shell: &[String], cmd: &str, cwd: &Path, timeout: Duration) -> ExecOutcome {
    let Some((exe, args)) = shell.split_first() else {
        return ExecOutcome::Spawn(std::io::Error::other("the configured shell is empty"));
    };
    let spawned = Command::new(exe)
        .args(args)
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => return ExecOutcome::Spawn(e),
    };
    let (done, drained) = mpsc::channel();
    forward_to_stderr(child.stdout.take(), done.clone());
    forward_to_stderr(child.stderr.take(), done);

    let outcome = match child.wait_timeout(timeout) {
        Ok(Some(status)) if status.success() => ExecOutcome::Ok,
        Ok(Some(status)) => ExecOutcome::Failed(status.code()),
        Ok(None) => {
            let _ = child.kill();
            let _ = child.wait();
            ExecOutcome::TimedOut
        }
        Err(e) => {
            let _ = child.kill();
            ExecOutcome::Spawn(e)
        }
    };
    let draining = Instant::now();
    let all_drained = (0..2).all(|_| {
        let left = DRAIN_GRACE.saturating_sub(draining.elapsed());
        drained.recv_timeout(left).is_ok()
    });
    if !all_drained {
        // The readers are left to finish (or not) on their own; they die
        // with wt.
        eprintln!("wt: exec left background processes holding its output: {cmd}");
    }
    outcome
}

/// Copies `pipe` to wt's stderr on its own thread and reports on `done`
/// when the pipe closes.
fn forward_to_stderr(pipe: Option<impl Read + Send + 'static>, done: mpsc::Sender<()>) {
    thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let _ = std::io::copy(&mut pipe, &mut std::io::stderr());
        }
        let _ = done.send(());
    });
}

/// Where `claude` lives on PATH: `which`'s own PATHEXT-extension search
/// (an npm-installed `claude` is a `.cmd` shim on Windows), the same lookup
/// `run_cli` uses for `az`/`gh`.
#[must_use]
pub fn find_claude() -> Option<PathBuf> {
    which("claude")
}

/// Runs the naming model's argv (`argv[0]` is the resolved `claude` path)
/// through `run_exe`: a `.cmd` shim gets std's batch-safe quoting, since the
/// argv carries a tracker title.
pub fn run_model(argv: &[String], cwd: &Path, timeout: Duration) -> Result<CliOutput, AppError> {
    let Some((exe, rest)) = argv.split_first() else {
        return Err(AppError::Cli("empty command".to_owned()));
    };
    run_exe("the naming model", Path::new(exe), rest, cwd, timeout)
}

/// An exit code as the messages print it: `None` is a signal.
#[must_use]
pub fn rc(code: Option<i32>) -> String {
    code.map_or_else(|| "signal".to_owned(), |c| c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ----------------------------------------------------------- TestExecShell

    #[test]
    fn every_pathext_extension_is_tried_in_each_dir_in_order() {
        let path = std::env::join_paths([Path::new("/a"), Path::new("/b")]).unwrap();
        assert_eq!(
            candidates("az", &path, Some(".EXE;;.CMD")),
            [
                Path::new("/a").join("az.EXE"),
                Path::new("/a").join("az.CMD"),
                Path::new("/b").join("az.EXE"),
                Path::new("/b").join("az.CMD"),
            ]
        );
        assert_eq!(
            candidates("az", &path, None),
            [Path::new("/a").join("az"), Path::new("/b").join("az")]
        );
    }

    #[test]
    fn az_is_found_as_its_cmd_shim() {
        let dir = std::env::temp_dir().join(format!("wt-which-{}", std::process::id()));
        let empty = dir.join("empty");
        let bin = dir.join("bin");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("az.cmd"), "@echo off\r\n").unwrap();
        let path = std::env::join_paths([&empty, &bin]).unwrap();
        assert_eq!(
            which_in("az", &path, Some(".exe;.cmd")),
            Some(bin.join("az.cmd"))
        );
        assert_eq!(which_in("gh", &path, Some(".exe;.cmd")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// az is `az.cmd`, so its argv goes through cmd.exe: a `%XX%` in a URL
    /// must not be expanded as a variable, nor `@file` touched.
    #[cfg(windows)]
    #[test]
    fn a_percent_encoded_url_survives_a_cmd_shim() {
        let dir = std::env::temp_dir().join(format!("wt-cmdshim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("echoargs.cmd");
        std::fs::write(&shim, "@echo %*\r\n").unwrap();
        let url = "https://dev.azure.com/contoso%20org/%C3%98konomi%20Prosjekt%PATH%/_apis?a=1&b=2";
        let args = [
            "--url".to_owned(),
            url.to_owned(),
            "--body".to_owned(),
            "@b.json".to_owned(),
        ];
        let out = run_exe("echoargs", &shim, &args, &dir, CLI_TIMEOUT).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.success, "{}", out.stderr);
        assert!(out.stdout.contains(url), "{}", out.stdout);
        assert!(out.stdout.contains("@b.json"), "{}", out.stdout);
    }

    /// A tracker title reaches `claude -p` through its `.cmd` shim: a quote,
    /// an `&` or a `%VAR%` in it must arrive as text, never run as cmd syntax.
    #[cfg(windows)]
    #[test]
    fn a_hostile_title_survives_the_claude_cmd_shim_as_text() {
        let dir = std::env::temp_dir().join(format!("wt-modelshim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shim = dir.join("claude.cmd");
        std::fs::write(&shim, "@echo %*\r\n").unwrap();
        let title = r#"a" & echo INJECTED & "b %PATH%"#;
        let argv = [
            shim.display().to_string(),
            "-p".to_owned(),
            title.to_owned(),
        ];
        let out = run_model(&argv, &dir, CLI_TIMEOUT);
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.unwrap();
        assert!(out.success, "{}", out.stderr);
        assert!(
            !out.stdout
                .lines()
                .any(|l| l.trim_start().starts_with("INJECTED")),
            "{}",
            out.stdout
        );
        assert!(out.stdout.contains("%PATH%"), "{}", out.stdout);
    }

    #[test]
    fn test_platform_default_is_usable() {
        let shell = default_shell();
        let exe = shell.first().unwrap();
        assert!(on_path(exe), "{exe} is not on PATH");
    }
}
