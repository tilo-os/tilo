# Task 1 — shared automatic native runtime attachment

Implementation committed as `c2d6045c84bbf57bed599dbbc1a374304643734a` (`feat: automatically attach native apps to shared runtime`). No push was performed. No GUI windows or actual app packages were launched in this task.

## Implementation and interfaces

`runtime_portal::native::ensure_attached(app_id: &str) -> io::Result<()>` is the shared Unix bootstrap. Complete inherited collaboration/identity Unix stream descriptors are authoritative, including when profile/binary overrides disagree. Partial, malformed, closed, aliased, stdio, or wrong-transport inherited descriptors fail rather than joining another runtime. The helper never mutates global process environment. It execs the native host, which prepares the existing identity handshake and connected descriptors and execs the original executable with opaque arguments and the original working directory. SDK adoption now restores CLOEXEC so later execs do not retain an app's collaboration connection.

Native GUI dispatch invokes the helper before default/custom construction and before ApplicationDefinition preparation. Handled commands, capture/matrix, audit and headless modes stay detached. A private resume marker prevents a false-returning command handler from executing twice across automatic exec. `LaunchSpec::runtime_attachment(fn(&LaunchArgs) -> bool)` defaults to true and provides the requested generic explicit exception for privacy/local modes. A false policy skips bootstrap even under inherited connections; it does not revoke those connections. Browser's separate local-only constructor/service suppression is owned by the Browser task, not this commit. No application allowlist or per-app integration was added here.

The background daemon owns the existing production `Session`, identity broker, collaboration broker, shared content store and authority. MCP clients use existing `McpServer` instances backed by that same broker. Neither an app's exit nor MCP EOF owns/terminates the daemon or the other independently launched apps. The original manifest mode retains its child-owning lifecycle.

CLI:

- `qos-runtime-mcp [--profile ABSOLUTE_PATH] --daemon`: foreground service owner (automatic startup launches this detached with stdio redirected).
- `qos-runtime-mcp [--profile ABSOLUTE_PATH] --connect`: stdio MCP proxy; starts/reuses the same runtime.
- `qos-runtime-mcp [--profile ABSOLUTE_PATH] --run-app APP_ID -- EXECUTABLE ARGS...`: attach and exec. `--profile` must precede `--run-app`; arguments after the separator, including `--apps`, belong to the app.
- Existing `qos-runtime-mcp --profile ABSOLUTE_PATH --apps MANIFEST` remains supported.

Profile resolution is CLI `--profile`, otherwise absolute `QOS_RUNTIME_PROFILE`, otherwise `$HOME/Library/Application Support/qOS/runtime` on macOS or `$HOME/.local/state/qos/runtime` on other Unix hosts. Native startup creates a missing private profile and rejects a pre-existing public profile, symlink, foreign owner, and unsafe lock/log file. Existing legacy Session directory hardening remains compatible. Existing public runtime lock files from older builds are rejected by the new private-file check rather than followed or reused unsafely.

`QOS_RUNTIME_BINARY` selects an absolute executable. Otherwise discovery prefers the app's sibling `qos-runtime-mcp`, then PATH. A compiled-in source checkout fallback serializes `cargo build -p shell --no-default-features --bin qos-runtime-mcp --target-dir TARGET` when the host is missing. TARGET is absolute/working-directory-resolved CARGO_TARGET_DIR or the checkout's target directory. The build uses dev/debug output, reports diagnostics on stderr, and is bounded to ten minutes including lock acquisition. Timed-out builds terminate their owned process group. Cargo uses its normal dependency/cache/network settings, including explicitly configured offline settings. The initial forced-offline implementation was corrected in fix round 1 below.

`QOS_RUNTIME_STANDALONE=1` explicitly disables bootstrap only when no inherited service descriptors exist. It is not the default and does not override inherited authority. `QOS_RUNTIME_RESUME_LIVE` is an internal automatic-exec marker, not a user configuration setting. The host sets `QOS_PACKAGE_ID`, `QOS_WORKSPACE_ID`, the two service descriptors and `QOS_BLOB_STORE` on its exec command. App XDG/state paths are preserved; the profile isolates runtime authority/content, not each app's ordinary document storage. Native proof should set separate ordinary XDG paths if app-state isolation is required.

The daemon's startup lock serializes contenders; readiness is an actual bounded service-socket response, not PID existence. The Session lock prevents a competing daemon from unlinking a live owner's endpoints. Stale native endpoints are removed only after Session ownership is acquired. Startup waits at most 15 seconds (individual probes 300 ms), native status replies are bounded to 16 KiB, MCP retains the existing 1 MiB line limit, and active daemon client workers are bounded to 64. Failures go to stderr and the profile's private daemon.log; they do not silently fall back to standalone.

## Verification and evidence

- `cargo test -p runtime-portal --test native`: **10 passed**. `portal-green.log`; RED evidence in `portal-red.log`, `alias-red.log`, `datagram-red.log`, `cloexec-red.log`, `offline-red.log`. Covers inherited precedence and malformed input, argv/cwd exec preservation, opt-out, wrong socket type, CLOEXEC adoption, serialized missing-host developer build and visible build failure. Developer build tests use an isolated controlled Cargo executable to exercise the real bootstrap build/locking path without rebuilding the workspace three times; the real host build is separately verified below.
- `cargo test -p ui --lib launch::tests`: **32 passed**. `ui-green.log`; `ui-red.log` and `policy-red.log` record expected failures. Covers default/custom/definition preconstruction attachment, all diagnostic exclusions, handled commands, inherited-fd command replay/local policy through isolated subprocesses, and the existing Photos argument-aware opener test. No real daemon or GUI is launched by the UI unit tests.
- `cargo build -p shell --no-default-features --bin qos-runtime-mcp`: **passed**. `host-build.log`.
- `cargo test -p shell --no-default-features --lib runtime_host`: **33 passed** in the final refreshed run, recorded in `host-green.log`. Includes original adapter/manifest/host tests plus seven real-process daemon tests: stale endpoint recovery, simultaneous first launch, reuse, MCP EOF persistence, profile lock contention, argv/cwd/--apps preservation, unsafe paths, and independently bootstrapped real CollaborationManager SDK publishers. The latter verifies first app discovery, late second app discovery with distinct peer IDs, one daemon PID, and surviving second-app registration after the first exits. Test-owned processes are killed/reaped and isolated profiles removed.

Observed failures were investigated, not waived. Standalone integration-test targeting causes Cargo to build unrelated Linux-only shell binaries on macOS (`click_test_client`, `qos-loadclient`, `shots`), so daemon process tests live under the shell library and invoke the explicitly built host binary. The initial cwd assertion was corrected for macOS `/tmp` canonicalizing to `/private/tmp`. Tightening the shared directory helper initially broke existing Session tests and new tempfile roots (0755 here); native strict validation is now separate, legacy hardening preserved, and native test roots explicitly 0700. `private-red.log` records rejection tests before this fix.

Lint limitations: an initial `-D warnings` package lint stopped in the unchanged collab-types checked-history large enum. `cargo clippy -p runtime-portal -p shell --no-default-features --lib --no-deps` also exposed existing shell/frame/MCP/process lints. New daemon Errors documentation and semicolon findings were fixed; the owned portal arithmetic findings were fixed. No unrelated lint cleanup or global formatting was performed. `clippy.log`, `clippy-scoped.log`, and `clippy-owned.log` retain the exact findings. This is not a claim that package-wide shell Clippy is green.

## Scope and remaining proof

Task 2 must still run actual independently launched word-processor, file-explorer, photos and an additional ordinary app, discover them through `--connect`, exercise a declared action to its canonical terminal result, retain native/visible-frame evidence, and verify surviving apps after one closes. The current evidence proves SDK/native transport and shared lifecycle without GUI, not full real-app operation or visual behavior. Browser private-mode suppression is a separately owned prerequisite for its privacy proof. Linux compilation/execution was not performed here.

Owned changes: runtime-portal native bootstrap/tests plus narrow lib additions; runtime_host daemon/process tests/module export, reusable Unix connection preparation/private file helper; qos-runtime-mcp CLI; shell dev-dependency/lock entry; and only our ui/launch.rs policy/hooks/tests. The initial launch baseline is retained as `launch-baseline.rs`/`.patch`; concurrent Photos/History setter and tests are preserved and excluded from this commit.

## Task 1 fix round 1 — reviewed descendant provenance and Cargo networking

Both Important findings in `task-1-review.md` were reproduced and fixed. This section supersedes initial-commit behavior for process provenance and Cargo networking. Fix commit: `6dce082e816f47720273bf88c8ce7a8872bcd041` (`fix: reattach ordinary native app descendants safely`). No application source changes, native GUI launches, push, subagents or broad lint loops were performed.

Automatic exec now publishes `QOS_RUNTIME_CONNECTION_OWNER_PID` and the actual selected `QOS_RUNTIME_PROFILE`. This PID is lifecycle/transfer metadata, not an authorization credential. `runtime-portal` recognizes a well-formed foreign owner as ancestor metadata and ignores its collaboration/identity descriptor values before both bootstrap validation and `service_fd` lookup. Thus a normal `Command` child of an SDK app obtains its own connections; a diagnostic/standalone/local child that skips bootstrap cannot adopt stale or reused descriptor numbers. The native-session heuristic likewise ignores an ancestor's workspace metadata.

No stamp preserves normal qOS/legacy inherited authority. A stamp naming the current process requires a complete valid transfer; a malformed/zero stamp or malformed/partial current/legacy descriptor transfer remains fail-closed. Negative descriptor integers are rejected before constructing a borrowed-fd wrapper. No process-global environment mutation or per-app environment clearing was introduced.

Shared `Connections::prepare` clears ancestor ownership/resume metadata whenever it makes a new transfer, including managed `Session::launch` where the eventual child PID is not known yet. The automatic `--run-app` exec stamps its exact unchanged PID after preparation. It restores command resumption only when the requesting bootstrap marker belongs to this same process and app. Existing shared GUI resume dispatch uses the now-provenance-aware `service_fd`, so an ordinary same-app child runs its own command handler once; its subsequent automatic exec skips only that invocation's replay. An explicit `--profile` overrides a conflicting ambient profile and is propagated to descendants.

Removed forced `--offline`. The missing-host build now honors normal Cargo configuration/network/cache behavior and preserves an explicit `CARGO_NET_OFFLINE`. The controlled compiler regression checks the ordinary argument list and both absent/default and explicit `CARGO_NET_OFFLINE=true` environments, retaining the concurrent serialized-build assertion.

RED evidence:

- `fix-1-portal-red.log`: four expected failures, covering forced-offline arguments, stale SDK descriptor lookup, stale standalone metadata, and unknown provenance acceptance.
- `fix-1-host-red.log`: actual SDK child failed on inherited non-socket metadata after ordinary parent `Command::spawn`; explicit profile propagation returned `/must-not-be-used` instead of the selected profile.
- `fix-1-ui-red.log`: same-app descendant incorrectly skipped its own command handler.

GREEN evidence:

- `cargo test -p runtime-portal --test native`: **13 passed**, `fix-1-portal-green.log`.
- `cargo test -p ui --lib launch::tests`: **32 passed**, `fix-1-ui-green.log`, including current resume, ordinary same-app descendant, explicit local policy and preserved Photos opener regression.
- `cargo build -p shell --no-default-features --bin qos-runtime-mcp`: **passed**, `fix-1-host-build.log`.
- `cargo test -p shell --no-default-features --lib runtime_host`: **36 passed**, `fix-1-host-green.log`. New real SDK-parent → ordinary Command same-app child test observes two distinct live broker registrations through one runtime, with no per-app clearing. Additional tests verify explicit-profile propagation and managed-transfer marker clearing. Previous independent launch/reuse/EOF/exit and MCP/Session regressions remain green.

The previously disclosed unrelated warnings/lint limitations remain scoped out. Actual ordinary GUI Cargo launches, real app action execution and visible native proof remain Task 2. Browser privacy/packaging changes belong to their separately committed work and were not staged here.
