# Write an isolated test

Tests must not depend on a contributor's preferences or reach their live
desktop. Environment isolation is useful protection, not an OS sandbox: hardware
access and code that opens explicit host paths need separate seams.

## Inject paths and stores first

Pass a root directory, store, connection or recording dispatcher to the
production path. Give each test its own temporary fixture. This avoids
process-global state and lets tests run concurrently.

Use [tilo-test-env](../../crates/dev/test-env/src/lib.rs) only when the
environment itself is the interface, such as an XDG resolver or launcher
environment contract. Add the `tilo-test-env` package as a dev-dependency with
the relative path to `crates/dev/test-env`.

- `ScopedEnv` sets/removes variables and restores their previous values on drop,
  including assertion unwinding.
- `TestHome` supplies a fresh HOME/XDG tree and a private runtime directory.
- Both hold the same process-wide, re-entrant lock. The lock protects only
  cooperating code; an unguarded reader or background thread can still race.
- `runtime_tempdir()` leaves room for Unix socket names; use it for socket
  fixtures instead of assuming an arbitrary temporary path is short enough.

Do not call `std::env::set_var` or `remove_var` directly in tests; Clippy's
disallowed-method rules direct callers to these fixtures.

Plain Cargo tests share one process per test binary; serial execution can help
diagnose interference but is not a replacement for removing shared state.
Nextest runs tests in separate processes. The
[ci profile](../../.config/nextest.toml) has no retries: investigate races,
rather than treating a later pass as success.

## Keep desktop services out of reach

The configured Cargo runner isolates test/bench harnesses and doctests on
x86_64/aarch64 Linux and on macOS. It does not isolate ordinary `cargo run`
programs, and this runner is not configured for Windows. See
[.cargo/config.toml](../../.cargo/config.toml) and
[tools/cargo-runner.sh](../../tools/cargo-runner.sh).

[tools/with-test-environment.sh](../../tools/with-test-environment.sh) provides
fresh HOME/XDG directories, a fixed locale/timezone/keyboard layout, no
display/session sockets, and D-Bus addresses pointing at absent sockets. It
preserves an intact enclosing test environment and removes its temporary tree
when the command exits.

A manually launched test binary bypasses Cargo's runner. Wrap it explicitly:

```sh
tools/with-test-environment.sh path/to/test-binary
```

Replace the path with the actual built test binary. Tests that launch
desktop-facing processes call `tilo_test_env::require_isolated_desktop()` before
doing so; the guard refuses an unisolated launch. Do not forge its environment
marker to suppress that refusal.

A test needing D-Bus starts its own private bus or uses a peer connection and
injects it. Never reconnect to the contributor's session bus, keyring, systemd
user manager or logind to make a test pass.

Shell tests of session scripts source
[tools/desktop-isolation.sh](../../tools/desktop-isolation.sh), call
`tilo_test_isolate "$@"`, and stage scripts with `tilo_stage_script`. The helper
rewrites named command variables and rejects remaining installed
Tilo/systemctl/loginctl paths. Inject fixture roots for sysfs, backlights and
other hardware state; a fresh HOME does not isolate those.

## Start only fixtures the test owns

On Unix, write shell fixtures with `tilo_test_env::script(path, body)`; run the
result through `Script::command()` or `Script::exec_argv()`. Invoking the shell
avoids intermittent `ETXTBSY` when a sibling fork retains a descriptor to a
just-written executable.

Use the crate's `wait_until`, `wait_for`, `recv` or `within` helpers to wait for
observable readiness with a hang bound, not arbitrary sleeps. Locate package
executables with `package_binary!` so relocated nextest archives work.

Bound subprocess lifetime and output. On success, failure and cancellation, stop
and reap owned children and release mounts, sockets and temporary stores. Do not
kill by a broad process-name pattern that can match another session.

## Declare required capabilities honestly

Use `tilo_test_env::require_capability!` for an environment-dependent probe. The
[capability definitions](../../crates/dev/test-env/src/capability.rs) name each
supported `TILO_REQUIRE_*` variable. Without the capability the macro reports a
skip; with its requirement set to `1`, absence fails.

A skip is not behavioral evidence. Checks qualifying GPU, audio, codecs, FUSE or
kernel behavior must require the relevant capability in their qualified
environment. Do not clear requirements, return early or substitute a fake
backend to turn missing coverage green. See
[troubleshooting](troubleshooting.md#the-required-runtime-or-capability-is-missing).
