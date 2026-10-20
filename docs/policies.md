# Project policies

Tilo's standing rules for contributors, human or agent, in one place. Each rule
says why it exists and how it is held:

- **Check**: a hook or CI step fails when the rule is broken. The path names
  the check; reading it is the precise definition of the rule.
- **Review**: nothing automated holds it yet. Reviewers and authors carry it,
  and a change that breaks it is fixed forward when noticed.

Where a rule is described in more depth elsewhere (`AGENTS.md`, `DEVELOPMENT.md`,
[`HIG.md`](../crates/sdk/ui-core/HIG.md), [`security.md`](architecture/security.md)),
this page links there instead of repeating it. A rule listed here as a check
and described differently elsewhere is defined by the check.

Most checks run in three places: `.githooks/pre-commit` and `.githooks/commit-msg`
locally (enable them with `python3 tools/dev/install-git-hooks.py`), and
**CI / fast quality** (`.forgejo/workflows/lint.yml`), whose policy suite is
[`tools/policy/run.sh`](../tools/policy/run.sh). Clippy, rustfmt over whole
targets and the tests run in **CI / affected packages**
(`.forgejo/workflows/check.yml`). The same checks are declared for [`t`](t.md) in `t.toml`.

## Exceptions

An exception is a reviewed change to the rule's own exception list, made in
the change that needs it, with the reason written next to the entry:

| Kind | Where the exception lives |
|---|---|
| A Clippy lint | `#[allow(clippy::…, reason = "…")]` on the narrowest item. `too_many_arguments` has no exception. |
| A non-text or non-LF file | `tools/policy/source_content_exceptions.json`, admitted only unchanged after it lands |
| A ratcheted count (UI tokens, app portability, resource/action contract, widget writes, document engine, Rust file length) | the baseline beside the check; counts only fall |
| A shell layer edge or shell dependency | `tools/policy/shell_layers_baseline.toml`, with its reason; the lists only shrink |
| A size budget | one raise file per change under `tools/perf/*-size-budget/raises/` |
| A licence, advisory or dependency source | `deny.toml`, with a comment saying why |
| An interpreter script in the OS image | a reasoned line in `tools/policy/image_interpreter_baseline.txt`; the list only shrinks |
| An old RustCrypto edge | `SANCTIONED` in `tools/policy/check_crypto_generation.py` |
| A review-only rule | say so, and why, in the commit message |

Changing a rule itself is a change that edits this page and the check
together.

## Process

| Rule | Why | Enforced by |
|---|---|---|
| Commit subjects are Conventional Commits: `type(scope)!: description`, with the types listed in `DEVELOPMENT.md` ("Pre-commit hook"). Git's own merge, revert and autosquash messages pass. | The subject is the changelog and the first thing a bisect shows. | Check: `.githooks/commit-msg` → [`check_commit_message.py`](../tools/policy/check_commit_message.py); CI re-checks every new commit, including `--no-verify` ones, with [`check_commit_policy.py`](../tools/policy/check_commit_policy.py) |
| The repository is public and self-contained: no internal tracker ids, private-document links or chat threads in tracked files, commit messages, pull request titles or branch names. Public pull requests (`#1234`) and commits are fine. | An outside reader can follow nothing behind a private link. State the reason itself. | Check: [`check_public_references.py`](../tools/policy/check_public_references.py) (pre-commit `--staged`, policy suite); `check_commit_policy.py` for messages and branch names; `check_commit_message.py --title` for pull request titles |
| Everyone, person or agent, pushes straight to `main` after running the checks their change affects with `t`. CI on `main` informs and never reverts; a stabilizer moves `stable` forward from mostly green `main` commits, with fixes going back to `main`, and releases come from `stable`. Only the stabilizer pushes to `stable`. | Landing stays fast for many contributors at once, and releasability is one role's job instead of a gate on every push. | Forge branch protection on `stable`; the rest is described in `DEVELOPMENT.md`, "Routine delivery workflow" |
| Small, coherent, tested increments; no work-in-progress or fixup pushes. | Each commit on `main` should build, test and revert on its own. | Review |
| The author owns a change until it is verified on `main`. Say which of implemented, merged, deployed and tested a change has reached; "merged" is not "works". | Unverified claims are how regressions reach users. | Review |
| An issue is a self-contained fix brief: it quotes the code and call sites it is about, states the fix plan, blast radius and acceptance, so whoever fixes it does not have to rediscover them. | Most issues are fixed by someone other than their author, often an agent. | Review |
| `Cargo.lock` is committed and never regenerated. Move one crate with `cargo update -p <crate> --precise <version>`; a lock-only change declares that command in `TILO_CARGO_UPDATE`. CI builds `--locked`. | A fresh resolve moves hundreds of versions and a break surfaces far from the change. | Check: pre-commit → [`tools/ci/check_lock_update.py`](../tools/ci/check_lock_update.py); `--locked` in CI |
| `crates/workspace-hack` matches the dependency graph after any dependency change. | Otherwise package selections compile shared dependencies twice. | Check: `hakari` in `t.toml`, in the push gate (`.githooks/pre-push`); `t fix hakari` regenerates it |
| Every CI job declares a timeout of at most three hours; remote workflow actions are pinned to a full commit SHA. | A job that needs longer is a job to split; a mutable tag runs whatever it points at today. | Check: [`check_workflow_timeouts.py`](../tools/policy/check_workflow_timeouts.py); [`check_workflow_actions.py`](../tools/policy/check_workflow_actions.py) through `os/tests/workflow_dependency_invariants.sh` |
| Per-push CI checks affected packages only. Full OS and distribution builds stay explicit or scheduled; get a development artifact from a manual dispatch with the smallest inputs. | Runner time is shared by every change in flight. | Review |

### Code review: Draft, decision needed

Review is optional and informational today: requests, approvals and change
requests never affect whether a change lands. A review policy (what
needs a second reader, and who) is still to be decided; until then nothing on
this page depends on review having happened.

## Repository hygiene

| Rule | Why | Enforced by |
|---|---|---|
| Every tracked blob is UTF-8 text with LF line endings. Binaries (images, screenshots, fonts, archives) stay out of Git except the pinned assets listed as exceptions; produce them at build time or fetch them by digest. | Text diffs, reviews and merges; binaries bloat every clone forever. | Check: [`check_source_content.py`](../tools/policy/check_source_content.py) (pre-commit on the staged index, policy suite, and every blob of every new commit through `check_commit_policy.py`) |
| No generated output in Git: nothing under `target/`, `node_modules/`, `__pycache__/` or `.pytest_cache/`, no files under retired source roots, no flat tooling outside `tools/<area>/`. | Generated files go stale and conflict. | Check: [`check_repository_layout.py`](../tools/policy/check_repository_layout.py) |
| Reports, logs and scratch notes from a working session are not committed; findings go in the commit message or the issue. | They describe one moment and rot. | Review (binary reports are caught by `check_source_content.py`) |
| Nothing names one developer's machine: no real home directories, private wrappers or scratch paths. Use placeholders such as `/var/home/you`. | Docs and scripts must work for everyone. | Check: [`check_setup_neutral.py`](../tools/policy/check_setup_neutral.py) |
| Every crate under `apps/`, `crates/` and `examples/` is a workspace member, exactly once, and on edition 2024. | A crate outside the workspace is never built or tested; an older edition compiles silently. | Check: [`check_workspace_members.py`](../tools/policy/check_workspace_members.py) through `scripts/check-workspace-complete.sh` (pre-commit, policy suite) |
| Every file under a crate's `tests/` is compiled by a test target. | With `autotests = false`, an undeclared module is silently never built. | Check: [`check_integration_test_modules.py`](../tools/policy/check_integration_test_modules.py) |
| Checked-in Cap'n Proto bindings match their schemas. | Hand-edited or stale bindings break the wire format. | Check: [`check_capnp_bindings.py`](../tools/policy/check_capnp_bindings.py) |
| No credentials, merge-conflict markers or `dbg!` in a commit. | A pushed secret is public at once and must be rotated. | Check: [`added_line_policy.py`](../tools/policy/added_line_policy.py) over the lines a change adds (pre-commit on the staged index, `scripts/scan-secrets.sh`, and every new commit through `check_commit_policy.py`, so `--no-verify` does not skip it). `dbg!` only in Rust outside `vendor/`; a credential-shaped fixture needs `example` or `placeholder` on its line. |
| `docs/codemap.md` lists every crate; published developer docs have valid links. | "Where is X" starts there. | Check: `tools/docs/codemap.py --check`, `tools/docs/developer_docs.py check` (policy suite) |

## Rust code

| Rule | Why | Enforced by |
|---|---|---|
| Formatted with rustfmt (edition 2024). Vendored and `@generated` sources are skipped. | No formatting noise in diffs. | Check: pre-commit; `tools/ci/check-changed-rustfmt.sh` in `lint.yml` |
| Clippy's `pedantic` group and the panic lints (`unwrap_used`, `expect_used`, `indexing_slicing`, `panic`, `todo`, …) are errors; tests may unwrap, expect, panic and index. | Production code returns errors instead of crashing the session. | Check: `[workspace.lints]` in `Cargo.toml`, `clippy.toml`; `tools/ci/clippy-check.sh` (`-D warnings`, all targets) in `check.yml` |
| Every `#[allow]` carries a `reason`, names Clippy lints with their `clippy::` prefix and sits where it takes effect. | An unexplained exception is unreviewable; a misplaced one silences nothing. | Check: [`tools/ci/check_changed_rust_allows.py`](../tools/ci/check_changed_rust_allows.py) (pre-commit and `lint.yml`); prefix and placement in pre-commit only |
| A Rust file stays under 3000 lines; a longer one may not outgrow its ceiling. Split it into cohesive modules rather than raise the ceiling. Vendored and `@generated` sources are skipped. | A file that long has no one subject left, conflicts with every change in flight and is no longer read whole. | Check: [`check_source_file_size.py`](../tools/policy/check_source_file_size.py) (ratchet): pre-commit for the files a commit touches, the policy suite for all; ceilings in [`source_file_size_baseline.toml`](../tools/policy/source_file_size_baseline.toml) only fall (`--write-baseline` lowers them) |
| A function takes at most eight parameters (aim for five or fewer), and nothing excuses it. | Long parameter lists hide a missing type. | Check: `clippy::too_many_arguments` at threshold 8; [`check_too_many_arguments.py`](../tools/policy/check_too_many_arguments.py) rejects every way to silence it |
| Banned calls name their replacement in `clippy.toml`: no `std::env::set_var`/`remove_var` (inject the path, or `tilo_test_env` in tests), no raw `reqwest` constructors (`tilo_net::http`), no raw file locks (`advisory_lock::FileLock`), no raw `AutoCommit` apply (`tilo_automerge::ApplyIncoming`). | Each replacement fixes a defect the raw call has. | Check: `disallowed_methods` in each `clippy.toml`; [`check_automerge_apply_contract.py`](../tools/policy/check_automerge_apply_contract.py) keeps the Automerge ban from being switched off |
| Comments say *why*, and only where it is not obvious from the code. | Comments that narrate the code drift from it. | Review |
| Errors are actionable: say what failed and what to do. Retries and waits are bounded, long operations can be cancelled, and secrets never appear in logs, panics or command lines. | A support request should start from the message, not from a debugger. | Review (panic lints above remove the worst case) |
| Use the shared UI and platform primitives, and improve them rather than writing a local copy. | A local copy forks behaviour that every other caller gets right. | Review; several boundaries are checked, see [UI and apps](#ui-and-apps) and [Architecture boundaries](#architecture-boundaries) |
| Never set `CARGO_TARGET_DIR`, and never share one target directory between worktrees. | It is hashed into the compiler cache key, and a shared directory hands one worktree the other's binary. | Review (`AGENTS.md`) |

## Architecture boundaries

Each of these keeps one subsystem's internals behind its API, so the subsystem
can change without editing its callers.

| Rule | Enforced by |
|---|---|
| Raw AEAD, KEM and password-KDF crates are used only by `tilo-seal`. | Check: [`check_crypto_dependencies.py`](../tools/policy/check_crypto_dependencies.py) |
| Document content is edited through `tilo-doc`, not through Automerge directly. | Check: [`check_document_engine_boundary.py`](../tools/policy/check_document_engine_boundary.py) (ratchet) |
| Toolkit widgets never write the document; their owner submits the edit as a resource action. | Check: [`check_widget_document_writes.py`](../tools/policy/check_widget_document_writes.py) (ratchet) |
| Settings are written through typed keys. | Check: [`check_settings_writes.py`](../tools/policy/check_settings_writes.py) |
| The toolkit reads host inputs and its clock through `ui::host` and `ui::time`, never from the process directly. | Check: [`check_ui_host_inputs_contract.py`](../tools/policy/check_ui_host_inputs_contract.py), [`check_ui_clock_contract.py`](../tools/policy/check_ui_clock_contract.py) |
| Apps keep record content in the store their host lends them. | Check: [`check_app_content_store.py`](../tools/policy/check_app_content_store.py) |
| Apps compose `ui` text widgets; shaping and glyph emission stay private to `ui`. | Check: [`check_app_text_contract.py`](../tools/policy/check_app_text_contract.py) |
| The shared desktop crates reach no compositor, transport or host stack. | Check: [`check_desktop_model_dependencies.py`](../tools/policy/check_desktop_model_dependencies.py) |
| Shell layers: the shell's service modules (workspace model, persistence, collaboration broker, network bridge, identity broker, runtime host) reach no compositor, chrome, launcher, overlay or renderer code; crates carved out of the shell depend on neither the shell nor the toolkit or a renderer; no new crate depends on the shell, and the shell gains no dependency unnoticed. Rendering and state stay decoupled, and each extraction from the shell has no new edge to undo first. | Check: [`check_shell_layers.py`](../tools/policy/check_shell_layers.py) (ratchet), reading `use` trees and `Cargo.toml` only. The [baseline](../tools/policy/shell_layers_baseline.toml) only shrinks: a removed edge or dependency fails until its line goes (`--write-baseline` drops it). A new shell dependency is a reasoned `[shell_dependencies.added]` entry; a new extracted crate is a line in `[extracted]` |
| The shared app runtime carries no OS internals and no unused crates. | Check: [`check_runtime_dylib_surface.py`](../tools/policy/check_runtime_dylib_surface.py) |
| Crates the browser builds for wasm32 name `workspace-hack` only for native targets. | Check: [`check_wasm_workspace_hack.py`](../tools/policy/check_wasm_workspace_hack.py) |
| Each interface between separately updated parts (app ↔ shell, app ↔ runtime, device ↔ device, store package formats) has one version number and a committed schema (`interface.toml`); an incompatible change bumps the number, and the two sides talk only at equal numbers ([compatibility](architecture/compatibility.md)). | Check: [`check_interfaces.py`](../tools/policy/check_interfaces.py): `check` in the policy suite (schema matches the code), `against` in t's `changes` check (an incompatible change since main bumps the version) |
| A program's own on-disk format is a `tilo_files::native::migration::Format`: a version, the oldest version it still brings up, and a forward-only step per version, each tested against a file the previous version wrote; a file is kept as a snapshot before it is migrated, and the chain of steps never shrinks ([compatibility](architecture/compatibility.md)). | Check: [`check_format_migrations.py`](../tools/policy/check_format_migrations.py): `check` in the policy suite, `against` in t's `changes` check |
| `os/components.toml` is the single list of binaries in the image; image builders copy exactly what they compile. | Check: [`check_component_manifest.py`](../tools/policy/check_component_manifest.py), [`check_image_builder_inputs.py`](../tools/policy/check_image_builder_inputs.py) |

## Dependencies and runtime

| Rule | Why | Enforced by |
|---|---|---|
| Dependency licences are compatible with distributing the system under AGPL-3.0: permissive and weak-copyleft licences, plus listed exceptions. | We must be able to ship the binary we build. | Check: `[licenses]` in [`deny.toml`](../deny.toml), `cargo deny` in `lint.yml` |
| Known security advisories fail the build; an acknowledged one carries its reasoning in `deny.toml`. | A vulnerable dependency is a vulnerable Tilo. | Check: `[advisories]` in `deny.toml` |
| Crates come from crates.io or the few forks listed in `deny.toml`; no wildcard versions. | Every input is pinned and attributable. | Check: `[sources]` and `[bans]` in `deny.toml` |
| A changed manifest declares no unused dependency. | Each one costs compile time and attack surface. | Check: `cargo machete` through `tools/ci/check-unused-dependencies.sh --changed` |
| A new dependency is justified in the commit that adds it: licence, provenance, maintenance, size and the platforms it builds for. | A dependency is code we ship and must keep patched. | Review (licence and source are checked above) |
| Tilo's own crates use the RustCrypto generation iroh uses (`ed25519-dalek`/`x25519-dalek` 3, `rand`/`rand_core`/`rand_chacha` 0.10, `sha2` 0.11, `hmac` 0.13, `chacha20poly1305` 0.11); the previous one arrives only through third-party crates. OS randomness comes from `getrandom::SysRng`. | One generation to audit; the old one leaves the lock when its last upstream moves. | Check: [`check_crypto_generation.py`](../tools/policy/check_crypto_generation.py) (direct edges in `Cargo.lock`; `SysRng` is review) |
| TLS is rustls. No OpenSSL or `native-tls` anywhere in the graph. | One certificate-verification path to review, and no C library to patch in every image. | Check: [`check_tls_stack.py`](../tools/policy/check_tls_stack.py) |
| No telemetry, analytics or crash-upload client (Sentry, PostHog, OTLP exporters, minidump capture) anywhere in the graph. | `docs/architecture/security.md` says Tilo's own components report nothing home; panics stay local and feedback is a report the person approves. | Check: [`check_no_telemetry.py`](../tools/policy/check_no_telemetry.py) |
| GStreamer is the only media stack: no in-tree codec crates (`openh264` from source, `ffmpeg-next`). | Hardware codecs first, and H.264 in software only through Cisco's licensed binary. | Check: [`check_media_stack.py`](../tools/policy/check_media_stack.py) |
| No image Tilo ships carries Cisco's OpenH264 binary or its GStreamer plugin; a device gets them only as the owner's own download (the media-codecs extension). Only the CI and developer build environments listed in the check may install them. | Cisco's patent licence covers only binaries the user downloads from Cisco ([codec-pack.md](../os/usr/share/doc/tilo/codec-pack.md)). | Check: [`check_media_stack.py`](../tools/policy/check_media_stack.py) (no Containerfile, `os/` package list or script installs them by name, enables `fedora-cisco-openh264`, runs `install-build-deps.sh` or builds its final stage on the CI builder) and [`verify-no-cisco-openh264.sh`](../os/scripts/verify-no-cisco-openh264.sh) (the resolved frozen RPM snapshot and the finished OS rootfs) |
| No interpreted language (Python, JavaScript) on the runtime path of the OS or an app where a compiled alternative exists. Build and CI tooling may use Python. The one Python program the image ships on purpose is umu-launcher (Proton); python3 is otherwise there only for the reviewed distribution packages that require it. | Startup time, footprint and one fewer runtime to secure. | Check: [`check_image_interpreters.py`](../tools/policy/check_image_interpreters.py) refuses a new Python, Node, Deno, Bun or Perl script, or a shell script that runs Python, under `os/usr`, `os/etc` or the host payload `dist/session`, and Python a Containerfile or `os/components.toml` installs; the [baseline](../tools/policy/image_interpreter_baseline.txt) only shrinks. In the finished rootfs [`verify-no-python-runtime.sh`](../os/scripts/verify-no-python-runtime.sh) fails the image on any file outside an RPM that is or runs Python except umu-launcher, and on any package depending on Python outside its reasoned list. Apps are review |
| Inputs are pinned: the Rust toolchain in `rust-toolchain.toml`, crates in `Cargo.lock`, actions by SHA, and image packages by snapshot. Builds of apps and images are reproducible from a commit. | A build that cannot be reproduced cannot be verified. | Partly: `--locked`, the action and image checks above, [`check_dev_image_contract.py`](../tools/policy/check_dev_image_contract.py); reproducibility itself is review |
| Apps are small: each shipped app and the shared runtime have size budgets that only fall without a raise file. | Apps download on demand and are not installed. | Check: `tools/perf/tapp_size_ratchet.py` and `tools/perf/runtime_size_ratchet.py` in `lint.yml` ([measurements](measurements/app-package-size.md)) |

## UI and apps

The product reasoning behind these is in `AGENTS.md` ("Project Scope") and the
[HIG](../crates/sdk/ui-core/HIG.md).

| Rule | Enforced by |
|---|---|
| Every capability is an action on a resource: what a click can do, a screen reader, a script and an agent can do through `tilo-actions`. | Partly: [`check_app_resource_action_contract.py`](../tools/policy/check_app_resource_action_contract.py) (every graphical app publishes resources and actions; the APIs the resource model replaced are banned in apps and examples, [resources.md §8](architecture/resources.md)) and [`check_atspi_endpoint_contract.py`](../tools/policy/check_atspi_endpoint_contract.py) (every app uses the production AT-SPI endpoint). That a given feature has its action is review. |
| Colours, spacing, radii and type come from the `Theme`; nothing is hard-coded. | Check: the ratchets in `crates/sdk/ui-core/tests/` (`design_token_audit.rs`, `spacing_token_audit.rs`, `typography_token_audit.rs`; HIG §9.13) |
| User-visible strings are Fluent keys, and every key a crate uses exists in each catalogue it ships. | Check: [`check_app_copy.py`](../tools/policy/check_app_copy.py); copy style ratchet `copy_audit.rs`. That no literal reaches the screen is review. |
| Copy says state, outcome or next step in the person's words: no narration, implementation words, machine ids, `{:?}` output, all caps, facts joined with a separator glyph ("·", "•", "|", dashes, " / ") or unicode symbols as icons; buttons named by outcome; one name per thing (HIG §4). | Check: [`check_copy_language.py`](../tools/policy/check_copy_language.py) over every catalogue, with the glossary [`copy_glossary.toml`](../tools/policy/copy_glossary.toml) and a shrink-only per-catalogue baseline; `tilo_copy_lint` rejects `{:?}` handed to a copy builder. |
| Shell and app surfaces are built from the design system's primitives: no hand-painted button, row or chip shapes, no separator-glyph joins or symbol icons in string literals, no raw colour literals (docs/design-language.md, "Primitives"). | Check: [`check_primitives.py`](../tools/policy/check_primitives.py) over production Rust in `crates/session/` and `apps/`, with a shrink-only per-file count baseline [`primitives_baseline.txt`](../tools/policy/primitives_baseline.txt). |
| Shell copy says Workspace, People, Collaborator, Participant, not room or peer. | Check: [`check_shell_copy_contract.py`](../tools/policy/check_shell_copy_contract.py) |
| Never hand-roll a text field, button, dialog or other control; extend the shared widget (HIG §8). | Review; the `raw_paint` ratchet catches painting outside widgets |
| Search, undo, history, device sync and theming are system services. Apps register search providers and never draw a search field (HIG §4a). | Review |
| The OS owns window chrome: apps draw no title bar. Windows never overlap or float. | Review |
| Apps start empty: no seeded data on the live path; sample data only behind `--shots` or a demo flag. | Review |
| Apps hold no platform code and declare every capability their code uses. | Check: [`check_app_portability.py`](../tools/policy/check_app_portability.py), [`check_app_capabilities.py`](../tools/policy/check_app_capabilities.py), `tilo-app check --lint --all` in `lint.yml` |
| UX dictates the code: a change starts from what the person sees and does. | Review |
| Input and accessibility parity: keyboard, pointer, touch and assistive technology all reach every function; layouts survive enlarged text, high contrast and small displays; looking good does not need an expensive GPU. | Review; check light, dark and high-contrast shots (`TILO_SHOTS_THEME`) |
| Durable user state is never lost silently: formats stay readable across versions, and a failed write or migration is reported and recoverable. | Review |

### Motion

**Motion follows the input.** Direct manipulation (drag, swipe, pinch)
animates continuously and hands its velocity to the settle. A pointer click
may animate at `fast` or `base`. A keyboard shortcut applies its result on
the next frame; only spatial context may move, at `fast` at most: Super+→
shows the focused window at once and slides the viewport only when the
target was off screen, Ctrl+Tab has no crossfade, and the workspace switch
keeps a `fast` direction cue. Repeated presses never queue. Agents and
scripts follow the keyboard rule. Reduced motion stays the global override.

A shortcut is a frequent expert action with a known result, so animating it
adds latency to every repetition, while pointer and finger motion explains
where things went. Scope: the shell and every toolkit widget it lays out.
Transitions carry their source (`ui::tokens::MotionSource`) from where the
operation runs, and `tokens.motion` picks the duration per source; the
specification is [window-management.md § Motion](architecture/window-management.md#motion)
with the durations in HIG §9.7.

**Animations are composed from the motion library.** Every animation in the
toolkit, the shell and the apps uses a named motion from `ui::motion` (HIG
§9.7), which decides its spring, its duration, its reduced-motion variant and
what a shortcut gets; a motion that is genuinely new goes into the library.
Check: [`check_motion_library.py`](../tools/policy/check_motion_library.py)
fails on easing math, ad-hoc springs and lerp-over-time outside the library;
content curves (video keyframes, paint physics) are its short exception list.

Check: `the_first_frame_after_a_chord_shows_the_final_layout` in
`crates/session/shell/tests/strip_navigation.rs` (the first frame after a
chord or action shows the final layout, and only the viewport may still move,
within `fast`). A new transition that ignores its source, and a literal
`Duration::from_millis` in an animation instead of `tokens::motion`
(HIG §9.13), are review.

## Security and sandboxing

The sandbox is the security boundary that makes uninstalled apps safe; never
weaken it for convenience ([security.md](architecture/security.md)).

| Rule | Enforced by |
|---|---|
| Every app launches through `tilo-launcher` (bubblewrap, Landlock, seccomp, network namespace); a launch without the full policy is refused. | Tests in `crates/packaging/launcher` (seccomp programs are pinned byte for byte, egress in `tests/netgate_egress.rs`); weakening the policy itself is review |
| Capabilities need an explicit grant; a grant reaches the sandbox at the next launch. | Tests (`crates/session/shell/tests/portal_grant_reaches_sandbox.rs`) and review |
| Developer overrides (fixed lock password, test control socket) and the dev image never reach a production image. | Check: [`check_dev_overrides_contract.py`](../tools/policy/check_dev_overrides_contract.py), [`check_dev_image_contract.py`](../tools/policy/check_dev_image_contract.py) |
| Every sealed image uses the one UKI pipeline; the dev kernel build follows the release build. | Check: [`check_uki_pipeline.py`](../tools/policy/check_uki_pipeline.py), [`check_kernel_dev_build.py`](../tools/policy/check_kernel_dev_build.py) |
| Sockets live under `$XDG_RUNTIME_DIR`, never a shared name in `/tmp`. | Review |

## Testing and verification

| Rule | Why | Enforced by |
|---|---|---|
| Never `#[ignore]` a failing test, never weaken an assertion (loosen a tolerance, `==` → `>=`, delete or comment out an `assert`), and assert on the production path rather than a tautological helper. | A failing test is the specified behaviour; fix the code. | Review (the covenant in `crates/session/shell/tests/concept_parity_spec.rs` applies to all tests). An `#[ignore]` without a reason fails Clippy's `ignore_without_reason` and pre-commit. |
| A check that cannot observe its subject fails, never skips. | A green check that saw nothing is worse than none. | Review; the UI ratchets fail when a scanned root yields no files |
| A file the image seeds once into persistent state (tmpfiles `C`, or `f` with content) keeps every version it ever shipped under `os/tests/seeded-state/<target>/`, and a test loads each through the production reader. | Such a file is never overwritten on update, so an upgraded machine keeps the schema it was installed with; a reader that drops an old schema breaks only machines that were installed earlier. | Check: [`check_seeded_state_history.py`](../tools/policy/check_seeded_state_history.py) (policy suite) |
| A test that pins something the user asked for carries `// USER REQUIREMENT (date): "<their words>"` above it, and keeps its assertions: no commit renames or deletes it, drops the marker, or changes, removes or reorders a call to an `assert…` macro or helper in its body, unless its message has a `User-approved: <what the user agreed to, and when>` trailer. Change the code to pass such a test. | The test is the requirement written down; weakening it to make code pass is how a behaviour the user already asked for once regresses unseen (window resizing went back to resizing the wrong window this way). | Check: [`check_user_requirement_tests.py`](../tools/policy/check_user_requirement_tests.py) on every new commit (`changes` in `t.toml`); helpers a pinned test calls are review |
| Every test suite outside Cargo is gated in CI or explains why not. | A suite nothing runs stops passing unnoticed. | Check: `tools/ci/suites.py check` (policy suite) |
| Tests never touch the developer's desktop or process environment. | Parallel tests race on globals; a test must not depend on the machine it runs on. | Partly: the `set_var` ban above; see `DEVELOPMENT.md` ("Tests never touch your desktop") |
| Tests are fast, deterministic and meaningful: no arbitrary sleeps, no "flaky, a rerun passed". Prefer seam and integration tests over unit tests that repeat them. | A flaky test trains everyone to ignore red. | Review |
| Production-path verification covers cancellation, interruption, reconnect and failure, not only the happy path. | Those are the paths users hit on a real network and a real laptop lid. | Review |
| Everything is drivable by agents, people and scripts through the semantic control surface (`AGENTS.md`, "Driving a running desktop"). | It is the surface users and agents depend on. | Review |
| UI changes come with screenshots in more than one theme; hardware claims come with evidence from that hardware. | Single-theme blind spots have shipped repeatedly. | Review |
| Performance changes report before and after numbers measured the same way; KPIs have budgets. | Speed is a product claim. | Partly: **Perf / scheduled** compares KPIs against `tools/perf/kpi-budgets.toml` nightly ([performance](measurements/performance.md)); it is not a required check, and per-change evidence is review |

## Legal and interoperability

| Rule | Enforced by |
|---|---|
| Interoperability with proprietary formats and protocols is clean-room: public specifications and black-box observation only. Nobody working on Tilo decompiles or disassembles proprietary binaries, or reads code derived from them. The same applies to a proprietary design system's unpublished formulas: reproduce what can be observed, not what was extracted. | Review |
| Ported or vendored code keeps its licence and notices and records its upstream version or commit (`vendor/PATCHES.md`, a `PROVENANCE.md` beside the code). | Partly: [`check_vendor_provenance.py`](../tools/policy/check_vendor_provenance.py) requires a licence file and a `PROVENANCE.md` naming the `Cargo.toml` version in every `vendor/*` directory; that the licence is the upstream text, and code ported outside `vendor/`, are review |
| No proprietary assets, trademarks or trade dress. | Review |
| No in-house H.264, HEVC or ProRes codec without a patent decision. | Partly: [`check_media_stack.py`](../tools/policy/check_media_stack.py) bans the codec crates; a hand-written codec is review |

## Candidates for automation

Review-only rules above that look cheap to check, not yet done:

- Motion: a ratchet for literal animation durations (the tab bar, spawn fade
  and strip indicators still carry their own constants).
