# wt

Rust CLI that turns an intent into a git worktree: a sibling directory, seeded and tracked.
Drop-in replacement for the Python original at `~/.config/wt`. `docs/feature-inventory.md` is
the behaviour reference; the spec at `docs/superpowers/specs/2026-09-28-wt-rust-design.md` is
the authority where the two disagree.

## Commands

- `cargo nextest run`: tests (use this, not `cargo test`)
- `cargo clippy --all-targets -- -D warnings`: must be clean; lints are `deny`, so this is the
  compile gate
- `cargo fmt --check`: formatting
- `cargo deny check`, `cargo machete`: dependency advisories, licenses, bans, unused crates
- `bacon clippy-all` / `bacon nextest`: watch mode
- `cargo run -- <args>`: run the CLI; set `WT_HOME` to a temp dir to keep your real config,
  sidecars and log clean
- `bd ready`: what is buildable next

The gate: fmt, clippy, nextest, all green before claiming anything works or closing a bead. The
Stop hook in `.claude/settings.json` runs it at the end of every turn and blocks while it is
red. CI runs the same gate plus deny and machete.

## Workflow

1. Brainstorm the feature (superpowers), then a spec and plan under `docs/superpowers/`.
2. Track the work in beads (`bd`).
3. Build it with TDD, one behaviour at a time: red, green, refactor.
4. When a design settles, fold the decisions into the tracked docs: `README.md` for behaviour
   and the reasoning behind it, `docs/invariants.md` for rules with the tests that pin them,
   and `CLAUDE.md` for the module map.

Specs, plans and beads are local only. They are gitignored and never pushed (never
`bd dolt push`). Anything that must outlive the machine goes into the three tracked docs.

## Decided, do not ask

Everything in `docs/conventions/` and in `Cargo.toml` is BK's standing preference: crates,
layout, architecture, testing, errors, style, release. Brainstorming, grilling and planning
sessions treat it as decided. Ask only about features, the domain, and real conflicts between
a feature and a convention; when you raise a conflict, name the convention.

## Hard rules

- Never weaken `[lints]` in `Cargo.toml` to make code compile. `#[allow(clippy::...)]` goes on
  one item only, with a one-line comment saying why. Ask BK before relaxing
  `arithmetic_side_effects` or `as_conversions`.
- `unsafe_code = "deny"`. The one FFI item, `disinherit_std_handles` in `src/main.rs`, carries
  `#[allow(unsafe_code)]`; ask BK before adding another.
- Pure core, thin IO shell: only `src/io/` and `src/main.rs` do IO. No traits, no mocks, no
  fakes.
- Never break an invariant in `docs/invariants.md`. An invariant without a test is a bug.
- Never accept a snapshot you have not read.
- No crate outside `docs/conventions/crates.md` without asking BK. Never `git2`; `gix` only if
  spawning `git` becomes a measured problem.
- No test needs network or touches the real `~/.config/wt` or the user's repos. `az`, `gh`,
  `herdr` and `claude` are never spawned in tests.

## Commits

Conventional Commits. release-plz builds `CHANGELOG.md` from the subjects, so a subject
describes the change for a user: never a bead id, never "this commit".

## Map

```
src/
  main.rs           clap, logging init, run(); anyhow only here          [wiring]
  error.rs          AppError (thiserror)
  domain/           pure: facts in, plan out                             [pure]
    paths.rs        WT_HOME / home -> config dir, log dir
    config.rs       DEFAULTS layer, parse, validate, deep-merge, resolve
    parse.rs        worktree porcelain, for-each-ref, reflog, remote URL
    naming.rs       slugify, fill, BRANCH_SPEC, model reply, name precedence
    select.rs       leaf, find_targets, select_one
    plan.rs         Step, plan_create, plan_remove, stale/merged decisions
    provider.rs     az / gh argv builders, JSON reply -> Outcome
    herdr.rs        herdr argv builders, reply parsers, labels, release_panes decision
    view.rs         ls table, complete list, dry-run text, init scripts
  io/               IO: gather, execute, roll back                       [IO]
    git.rs          spawn git: utf-8, null stdin, timeout
    cli.rs          spawn az / gh / herdr / claude / exec steps
    run.rs          gather facts, execute steps, roll back
tests/
  common/mod.rs     Env: temp repo with a bare origin, isolated git, WT_HOME in a temp dir
```

Module boundaries may shift; the pure/IO split does not.

## Conventions

Read the one that matches what you are about to do:

- `docs/conventions/architecture.md`: before adding a module, a trait, or anything with IO.
- `docs/conventions/testing.md`: before writing or changing a test.
- `docs/conventions/errors.md`: before adding an error variant, a log line, or output.
- `docs/conventions/style.md`: before writing code; the lint cheat sheet is there.
- `docs/conventions/crates.md`: before adding a dependency.
- `docs/releasing.md`: before touching versions, tags, or release config.
