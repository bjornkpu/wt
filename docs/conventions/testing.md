# Testing

TDD, one behaviour at a time: write the test, watch it fail for the right reason, write the
least code that passes, refactor while green. Every non-trivial branch, parser, or state
transition leaves a test behind. If a parser or decision changed and no test changed,
something is missing.

Run with `cargo nextest run`, never `cargo test`.

## Layers

- **Pure unit tests**, in-module (`#[cfg(test)] mod tests`) in `src/domain/`. Most tests live
  here. Plain data in, assert on plain data out.
- **Plan tests**: for decisions that return steps, assert the exact `Step` sequence for each
  verb and each policy branch.
- **Text snapshots**: inline `insta` (`@"..."`) for output a user reads: the `ls` table,
  dry-run text, and `init` scripts.
- **Integration tests** in `tests/`: run the real binary through
  `env!("CARGO_BIN_EXE_wt")`, with real `git` and `WT_HOME` in a temp dir. The shared harness
  is `tests/common/mod.rs`.

## Snapshots

Review with `cargo insta pending-snapshots` or `cargo insta review`. Accept only after
reading the new content. Never accept a snapshot you have not read, and never accept one to
make a red test green without understanding the diff.

## Integration test isolation

Nothing in a test may touch the user's real config, repos, or home. Point `WT_HOME` at a temp
dir. When a test runs git, isolate it from the user's git config so hooks and signing never
run. `Env::command` in `tests/common/mod.rs` does this for every spawn:

```rust
pub fn command(&self, program: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", self.root.join("gitconfig"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "BK")
        .env("GIT_AUTHOR_EMAIL", "bk@example.com")
        .env("GIT_COMMITTER_NAME", "BK")
        .env("GIT_COMMITTER_EMAIL", "bk@example.com")
        .env("WT_HOME", self.root.join("wt-home"))
        .env("PATH", self.hidden_path(&["herdr"]));
    cmd
}
```

Start every new file in `tests/` with `#![cfg(test)]`, so the `clippy.toml` test allowances
(`unwrap`, `expect`, `panic`, indexing) apply to its helpers too. `tests/common/mod.rs` has it;
not every older test file does yet.

## What never runs in a test

Network calls, paid APIs, or external CLIs whose output is not deterministic: `az`, `gh`,
`herdr` and `claude` are never spawned in tests. Their argument building and reply parsing are
pure and tested with plain strings. wt has no boundary traits or fakes (see
`architecture.md`): an integration test that reaches one of these calls takes the tool off
`PATH` with `Env::hidden_path` and asserts on the fallback.

## Later, when a bug motivates it

- `proptest` for parsers and state machines, once a bug shows example tests missed a case.
  Ask BK first: it is not a dependency yet.
- `cargo mutants` as a manual check for code no test would notice changing. Not in CI; it is
  slow.
