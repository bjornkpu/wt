# wt

Rust CLI that turns an intent into a git worktree: a sibling directory, seeded and tracked.
Drop-in replacement for the Python original at `~/.config/wt`. `docs/feature-inventory.md` is
the behaviour reference; the spec at `docs/superpowers/specs/2026-09-28-wt-rust-design.md` is
the authority where the two disagree. Work is tracked in beads (`bd ready`), not in docs.

## Commands

- `cargo nextest run`: tests (use this, not `cargo test`)
- `cargo clippy --all-targets`: must be clean; lints are `deny`, so this is the compile gate
- `cargo fmt --check`: formatting
- `bacon clippy` / `bacon nextest`: watch mode
- `cargo run -- <args>`: run the CLI; set `WT_HOME` to a temp dir to keep your real config and
  sidecars clean

Before claiming anything works: clippy, fmt, nextest, all green.

## Commits

Conventional Commits. Subjects describe the change for a user, never a bead id.

## Lints

`[lints.clippy]` in `Cargo.toml` is pedantic + nursery + panic-denying lints. Never weaken it to
make code compile. No `unwrap`/`expect`/`panic`/`todo`/indexing/`as` casts in non-test code.
`clippy.toml` allows them in tests. `#[allow(clippy::...)]` needs a one-line comment saying why,
on one item only. Ask BK before relaxing `arithmetic_side_effects` or `as_conversions`.

## Dependencies

Used: `clap` (derive), `clap_complete` (`unstable-dynamic`), `anyhow`, `thiserror`, `serde`,
`serde_json`, `toml`, `jiff`, `tracing`, `tracing-appender`, `tracing-subscriber`,
`percent-encoding`, `strsim`, `wait-timeout`; dev: `insta`. Add one when a task needs it. Ask
before adding anything else. Never `git2`; `gix` only if spawning `git` becomes a measured
problem.

## Style

Edition 2024 idioms (`let ... else`, let chains, `?`, iterators), `thiserror` `AppError` in
`src/error.rs`, `anyhow` only in `main`, `#[must_use]` on pure functions, `&str`/slices in
parameters. Ponytail rule: smallest working change, no traits with one implementation, no config
for constants. Every non-trivial branch or parser leaves a test.

## Architecture

Pure core, thin IO shell: the shell gathers facts, a pure function decides, the shell executes.
Pure modules take and return plain data ("facts in, plan out"); no traits, no mocks or fakes.

```
src/
  main.rs           clap, logging init, run(); anyhow only here
  error.rs          AppError (thiserror)
  domain/           pure: facts in, plan out
    paths.rs        WT_HOME / home -> config dir, log dir
    config.rs       DEFAULTS layer, parse, validate, deep-merge, resolve
    parse.rs        worktree porcelain, for-each-ref, reflog, remote URL
    naming.rs       slugify, fill, BRANCH_SPEC, model reply, name precedence
    select.rs       leaf, find_targets, select_one
    plan.rs         Step, plan_create, plan_remove, stale/merged decisions
    provider.rs     az / gh argv builders, JSON reply -> Outcome
    herdr.rs        herdr argv builders, reply parsers, labels, release_panes decision
    view.rs         ls table, complete list, dry-run text, init scripts
  io/               IO: gather, execute, roll back
    git.rs          spawn git: utf-8, null stdin, timeout
    cli.rs          spawn az / gh / herdr / claude / exec steps
    run.rs          gather facts, execute steps, roll back
```

Module boundaries may shift during implementation; the pure/IO split does not. Only `io/git.rs`,
`io/cli.rs`, `io/run.rs` and `main.rs` do IO. Everything else takes and returns plain data, so it
is tested directly without mocks.

## Testing

TDD: red, green, refactor, per behaviour.

1. Pure unit tests in-module (`#[cfg(test)] mod tests`).
2. Plan tests assert the `Step` sequence for each verb and each policy branch.
3. Snapshots: inline `insta` for the `ls` table, dry-run text, and `init` scripts. Never accept
   a snapshot you have not read.
4. Integration tests in `tests/`, real `git`, `WT_HOME` set to a temp dir, git isolated
   (`GIT_CONFIG_GLOBAL`, `GIT_CONFIG_NOSYSTEM=1`, fixed author/committer env). Never touch the
   real `~/.config/wt` or the user's repos.
5. `az`, `gh`, `herdr` and `claude` are never spawned in tests. Their argv and reply parsing are
   pure and tested with plain strings.

Gate before any bead closes: `cargo fmt --check`, `cargo clippy --all-targets`,
`cargo nextest run`, all green.
