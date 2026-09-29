# Errors, logging, output

## Errors

- Everything below `main` returns `Result<_, AppError>`. `AppError` is one enum in
  `src/error.rs`, built with `thiserror`.
- `anyhow` only in `main.rs`, for context at the boundary (`.context("reading config")`).
- A variant's message says what went wrong and, when there is one, what to do about it:
  `"cannot find the home directory; set WT_HOME"`.
- Wrap foreign errors with `#[from]` when the conversion is lossless, or a variant with fields
  when the caller needs to know which file or which command failed.
- Never swallow an error. `let _ =` on a `Result` needs a comment saying why ignoring it is
  right (for example, the child is already gone after a timeout kill).
- Per-item failures in a batch (one tree of many in a sweep) log a warning and continue.
  Failures that make the result wrong (the config, git itself) are fatal.
- No panics in non-test code: the lints deny `unwrap`, `expect`, `panic`, indexing and
  unchecked arithmetic.

## Logging

wt differs from the template here: logging is always on, at `info`, with no env var to set
the level.

- `tracing` macros everywhere; `tracing-subscriber` and `tracing-appender` set up once in
  `init_logging` in `src/main.rs`.
- Logs go to `wt.log` in the log dir (`WT_HOME`, or `~/.local/state/wt`), never stdout
  (INV-1). stderr is for errors and progress the user must see.
- A log dir that cannot be made only warns on stderr; the command still runs.
- Log what a later debugging session needs: commands spawned, files written, decisions taken.
  `warn!` on every failure that does not stop the run.

## Output

- stdout carries only what the command produces, so the shell wrapper can read it: the path
  for `new`, `item`, `pr`, `branch`, `cd` and `hook-create`; the listing for `ls` and
  `complete`; nothing for `open` and `rm`.
- A `--json` output, when a tool has one, is stable: add fields, never rename or remove them.
- Errors print once, from `main`, as `wt: <error>`.
