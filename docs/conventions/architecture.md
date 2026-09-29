# Architecture

## Pure core, thin IO shell

The shell gathers facts, a pure function decides, the shell executes. Facts in, plan out.

- `src/domain/` is pure. It takes and returns plain data: no filesystem, no network, no
  processes, no clock, no environment variables. Anything it needs from the world arrives as
  a parameter (the clock arrives as `now: jiff::Timestamp`, the environment as a lookup
  function in `paths::resolve`).
- `src/io/` does all IO: `git.rs` spawns git, `cli.rs` spawns az, gh, herdr, claude and exec
  steps, `run.rs` gathers facts, executes steps and rolls back.
- `src/main.rs` is wiring: parse arguments, start logging, gather facts through `io`, call
  `domain`, execute the result through `io`. `anyhow` lives here and nowhere else.

Because `domain` is plain data, it is tested directly with no mocks and no fakes. When a
decision is hard to test, the IO has leaked into it: move the IO out and pass its result in.

A decision with several effects returns them as data (a `Vec<Step>`, a plan struct) for `io`
to execute. Tests assert on the plan; one integration test proves the executor runs it.

## Traits: none

wt differs from the template here. It has no traits and no fakes, not even at a service
boundary. az, gh, herdr and claude are slow or non-deterministic, so their argv builders and
reply parsers are pure functions in `src/domain/` (`provider.rs`, `herdr.rs`, `naming.rs`),
tested with plain strings. Only `src/io/cli.rs` spawns them, and no test does: the
integration tests take them off `PATH` (see `testing.md`).

Local things are not boundaries. git and the filesystem are used for real in tests, inside a
temp dir (see `testing.md`).

## Typestate

When a value moves through states that must never be mixed at runtime (unvalidated then
validated, draft then sent), make each state its own type and make the transition a function
that consumes one and returns the next. The compiler then rejects the mix-up.

## Paths

`WT_HOME` overrides everything: config and logs both live in that one dir. Without it, wt
uses `~/.config/wt` for config and `~/.local/state/wt` for logs. `src/domain/paths.rs`
decides this.
