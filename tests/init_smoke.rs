mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::Env;

/// Quotes `s` as a POSIX single-quoted literal, for embedding a path in a
/// zsh `-c` command line.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[test]
fn pwsh_smoke_wt_cd_moves_the_shell() {
    if Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit"])
        .output()
        .is_err()
    {
        println!("skipping: pwsh not found on PATH");
        return;
    }

    let env = Env::new("init-pwsh-smoke");
    let target = env.make_tree("feat/x", "new");

    let init = env.wt(&["init", "pwsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.ps1");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let ps_command = format!(
        ". '{}'; Set-Location -LiteralPath '{}'; wt cd feat/x | Out-Null; (Get-Location).Path",
        script_path.display(),
        env.repo().display(),
    );
    let out = env
        .command("pwsh", &env.repo())
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let moved_to = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&moved_to), target, "{moved_to}");
}

#[test]
fn pwsh_smoke_a_relative_name_never_cds() {
    // A decoy directory named "zzz" exists relative to cwd; the sole tree
    // "wt complete" offers is also called "zzz". Only an absolute path may
    // cd, so the wrapper must leave the shell in the repo, not the decoy.
    if Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit"])
        .output()
        .is_err()
    {
        println!("skipping: pwsh not found on PATH");
        return;
    }

    let env = Env::new("init-pwsh-relative");
    env.make_tree("zzz", "new");
    std::fs::create_dir_all(env.repo().join("zzz")).unwrap();

    let init = env.wt(&["init", "pwsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.ps1");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let ps_command = format!(
        ". '{}'; Set-Location -LiteralPath '{}'; wt complete | Out-Null; (Get-Location).Path",
        script_path.display(),
        env.repo().display(),
    );
    let out = env
        .command("pwsh", &env.repo())
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ended_at = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&ended_at), env.repo(), "{ended_at}");
}

#[test]
fn pwsh_smoke_strict_mode_handles_no_output_and_bare_help() {
    // Set-StrictMode -Version Latest turns an out-of-bounds array index into
    // a terminating error; the wrapper must never index `$args`/`$out` that
    // way (item 2). `wt complete` outside a repo prints nothing on stdout, a
    // bare `wt` prints usage and exits 2 - neither may throw in the shell.
    if Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit"])
        .output()
        .is_err()
    {
        println!("skipping: pwsh not found on PATH");
        return;
    }

    let env = Env::new("init-pwsh-strict");
    let init = env.wt(&["init", "pwsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.ps1");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();
    let outside = env.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();

    let ps_command = format!(
        "Set-StrictMode -Version Latest; . '{}'; Set-Location -LiteralPath '{}'; \
         wt complete | Out-Null; wt | Out-Null; Write-Output done",
        script_path.display(),
        outside.display(),
    );
    let out = env
        .command("pwsh", &outside)
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
}

#[test]
fn zsh_smoke_a_relative_name_never_cds() {
    if Command::new("zsh").args(["-c", "exit"]).output().is_err() {
        println!("skipping: zsh not found on PATH");
        return;
    }

    let env = Env::new("init-zsh-relative");
    env.make_tree("zzz", "new");
    std::fs::create_dir_all(env.repo().join("zzz")).unwrap();

    let init = env.wt(&["init", "zsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.zsh");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let zsh_command = format!(
        "source {}; cd {}; wt complete >/dev/null; pwd",
        sh_quote(&script_path.display().to_string()),
        sh_quote(&env.repo().display().to_string()),
    );
    let out = env
        .command("zsh", &env.repo())
        .args(["-c", &zsh_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ended_at = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&ended_at), env.repo(), "{ended_at}");
}

#[test]
fn zsh_smoke_wt_cd_moves_the_shell() {
    if Command::new("zsh").args(["-c", "exit"]).output().is_err() {
        println!("skipping: zsh not found on PATH");
        return;
    }

    let env = Env::new("init-zsh-smoke");
    let target = env.make_tree("feat/x", "new");

    let init = env.wt(&["init", "zsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.zsh");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let zsh_command = format!(
        "source {}; cd {}; wt cd feat/x >/dev/null; pwd",
        sh_quote(&script_path.display().to_string()),
        sh_quote(&env.repo().display().to_string()),
    );
    let out = env
        .command("zsh", &env.repo())
        .args(["-c", &zsh_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let moved_to = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&moved_to), target, "{moved_to}");
}

#[test]
fn pwsh_smoke_wt_rm_steps_out_and_removes_the_tree() {
    // `-q` before the verb (item 3: verb detection skips global flags) must
    // still find "rm" and step out to the main checkout before the real
    // `wt rm` runs - Windows refuses to remove a directory while any
    // process, this pwsh session included, holds it as a current directory.
    if Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit"])
        .output()
        .is_err()
    {
        println!("skipping: pwsh not found on PATH");
        return;
    }

    let env = Env::new("init-pwsh-rm");
    let new_out = env.wt(&["-q", "new", "feat/rm-probe"]);
    assert!(new_out.status.success(), "{}", Env::stderr(&new_out));
    let target = PathBuf::from(Env::stdout(&new_out).trim_end());

    let init = env.wt(&["init", "pwsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init-rm.ps1");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let ps_command = format!(
        ". '{}'; Set-Location -LiteralPath '{}'; wt -q rm feat/rm-probe | Out-Null; (Get-Location).Path",
        script_path.display(),
        target.display(),
    );
    let out = env
        .command("pwsh", &target)
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ended_at = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&ended_at), env.repo(), "{ended_at}");
    // Not `!target.exists()`: git (or antivirus scanning the just-created
    // tree) can transiently hold the now-empty directory even after a
    // successful removal, which `wt rm` already tolerates as a warning
    // (`drop_empty_dir`) rather than a failure. The tree being gone means
    // it is no longer a registered worktree.
    assert!(env.trees().is_empty(), "{:?}", env.trees());
}

#[test]
fn zsh_smoke_wt_rm_steps_out_and_removes_the_tree() {
    if Command::new("zsh").args(["-c", "exit"]).output().is_err() {
        println!("skipping: zsh not found on PATH");
        return;
    }

    let env = Env::new("init-zsh-rm");
    let new_out = env.wt(&["-q", "new", "feat/rm-probe"]);
    assert!(new_out.status.success(), "{}", Env::stderr(&new_out));
    let target = PathBuf::from(Env::stdout(&new_out).trim_end());

    let init = env.wt(&["init", "zsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init-rm.zsh");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    let zsh_command = format!(
        "source {}; cd {}; wt -q rm feat/rm-probe >/dev/null; pwd",
        sh_quote(&script_path.display().to_string()),
        sh_quote(&target.display().to_string()),
    );
    let out = env
        .command("zsh", &target)
        .args(["-c", &zsh_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ended_at = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert_eq!(Path::new(&ended_at), env.repo(), "{ended_at}");
    assert!(env.trees().is_empty(), "{:?}", env.trees());
}

#[test]
fn pwsh_smoke_wt_cd_follows_a_non_ascii_path() {
    if Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", "exit"])
        .output()
        .is_err()
    {
        println!("skipping: pwsh not found on PATH");
        return;
    }

    let env = Env::new("init-pwsh-utf8");
    let target = env.make_tree("feat/blåbær", "new");

    let init = env.wt(&["init", "pwsh"]);
    assert!(init.status.success(), "{}", Env::stderr(&init));
    let script_path = env.root.join("wt-init.ps1");
    std::fs::write(&script_path, Env::stdout(&init)).unwrap();

    // Compared inside pwsh: its own stdout goes out in the console codepage.
    let ps_command = format!(
        ". '{}'; Set-Location -LiteralPath '{}'; wt cd feat/blåbær | Out-Null; (Get-Location).Path -eq [IO.Path]::GetFullPath('{}')",
        script_path.display(),
        env.repo().display(),
        target.display(),
    );
    let out = env
        .command("pwsh", &env.repo())
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_command])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "True");
}
