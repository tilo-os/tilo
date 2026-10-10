# Terminal editable-resource contract

The guarded native producer and Terminal/ush source integration are now a
review candidate. They are not an installed or fully qualified desktop backend.
The baseline inspection at `1196d1340bfef9c6b72305e4b1677f108add04bc` found no
cooperative producer. The production-path native qualification below closes
that source gap; managed-shell and mounted desktop acceptance remain separate.
Microphone consent cannot substitute for either.

## Baseline boundary

`apps/terminal/src/resource.rs` exposes session inspection and Send, Run, Paste
and Interrupt effects. `TerminalWidget::perform` in `main_body.rs` sends process
stdin; Run adds a carriage return and Paste can normalize newlines into carriage
returns. Those effects cannot implement an editable-buffer replacement.
Ordinary IME commits also write PTY bytes. The terminal transcript is read-only
accessibility output, accurately distinct from an input object.

The current IME stamp tracks cursor/input epoch and alternate-screen state. It
does not identify an inner selection, privacy mode or accepted edit. Terminal
therefore keeps Terminal purpose and dictation refusal. PTY ECHO/canonical flags,
foreground process groups and process-controlled OSC markers cannot certify a
public editable field. Preserve the existing agent refusal for the device's own
shell; adding a resource must not broaden that authority.

`crates/runtime/ush/src/supervisor.rs` chooses an arbitrary configured shell,
attaches its stdio to a PTY and returns the master descriptor through the
session's existing launch/lifetime route. Its management broker does not own
readline state. A Terminal-only trait or a mocked announcement cannot establish
that an actual shell editor accepted or undid text.

## Cooperative producer

The first supported producer must integrate with the actual managed shell's
line-edit loop, such as a qualified packaged Bash/readline adapter. Arbitrary
shells, TUIs and remote sessions without that producer remain unavailable.
Review the producer's licence and source/build pin and test its real buffer.
Do not silently change the user's selected shell.

Extend the existing ush session custody with a bounded, versioned editor FD
capability. Bind it to session generation, qualified producer identity and
lifetime. Do not add an ambient public socket or reuse the management audit as a
text channel. The existing single-FD launch response and EOF/reaping protocol
need an explicit extension; appending edit JSON to today's lifetime reader is
not compatible. Prevent descriptor inheritance into arbitrary children and
authenticate the actual sending process. Revoke before command acceptance or
yielding to a child.

The producer supplies an immutable lease containing field generation, revision,
UTF-8 selection boundaries, declared public/private mode, purpose, finite expiry,
capacity and supported operations. Unknown/protected mode exposes unavailable
status without surrounding text. Text, selection and keyed stamps stay in an
OS-owned participant-private, nonpersistent resource projection; they must not
enter the shared process transcript, collaboration history, telemetry or audit.
The existing resource/privacy policy must support that projection before it is
implemented.

## Editing and undo

Expose the live editor as a child of the existing terminal session resource.
Its inspection and guarded replacement use the same action/effect door as
accessibility and automation, with existing actor, role and foreground policies.
The same replacement handles native IME and dictation. Preedit stays local;
cancellation causes no producer edit.

ReplaceSelection compares the expected lease and selection in the actual
producer event loop, changes only that public buffer and returns an authenticated
applied receipt with its new revision. It cannot press Enter, execute, emit
escape codes, access a clipboard or fall back to Send/Run/Paste. Host-side
preflight alone does not fence a racing keypress.

A transaction ID is idempotent only for the identical lease and payload and
has a queryable outcome. A missing receipt becomes explicit uncertain delivery;
never blindly retry or redirect to another field. Keyboard/selection changes,
focus loss, target/process/session replacement, command acceptance, lock,
read-only or privacy transitions invalidate the original lease even if the
pixels and caret are unchanged. Reuse the compositor's native field identity,
edit stamp and dictation actor rather than adding another recognizer.

One applied insertion creates one system Undo group. Dispatch guarded restoration
to an actual producer checkpoint or inverse edit, with an acknowledged result.
Subsequent edits or command acceptance fence obsolete checkpoints. Undo never
reverses execution, process output or PTY writes. A producer without genuine
guarded restoration cannot qualify complete dictation support.

## Production-path acceptance

A spawned managed shell must expose its real public edit buffer, atomically
replace a Unicode selection without execution and undo through the semantic
resource actions. Cover typing/selection races, focus away and back, protected
input, TUI transitions, target/session replacement, command submission, disconnect
before and after acknowledgment, duplicate transactions, cancellation and
repeated sessions. Assert the actual producer buffer and accepted revision;
capturing PTY bytes or testing a host-only helper is insufficient. Include hosted,
standalone and embedded Terminal surfaces and preserve own-shell agent refusal.

## Qualified native editor feasibility fixture

`dictation-bash-editor-review.json` records ten actual Bash/readline journeys in
an isolated namespace with a controlling PTY, plus refusal to run qualification
with Python assertions disabled. The source fixture is
`tools/release/fixtures/bash_editor_review.c`; its runner is
`tools/release/fixtures/bash_editor_journey.py`. They are review inputs, not an
installed Terminal backend or an extension of the current ush launch protocol.

The fixture edits the real native buffer through Readline, replaces a Unicode
active selection, groups one native Undo and restores the selection. Actual
keypress and redisplay hooks invalidate an older revision even when text and
caret return to their original state. Newline and escape payloads are rejected;
the replacement never submits a command. A real child exec verifies that the
editor descriptor is close-on-exec. Interactive `read -e`, silent `read`, and a startup
`read -e` prompt, are refused. Crucially, the startup case failed when the
prototype checked only Bash's top-level `executing` flag: startup builtin input
occurs outside that loop. The corrected fixture also checks the actual builtin
execution witness. The failing receipt remains in private review evidence.

The installed idle event-hook route failed to consume typed input in this
review; the positive fixture polls the owned editor FD through the supported
Readline input-function hook instead. It initializes Bash's own Readline
integration before capturing hooks. The active selection is produced with
Readline's actual exchange-point-and-mark command, rather than assuming that
setting a mark creates a visible selection.

Reproduce only on the isolated review builder. Extract the exact ledger-pinned
signed development packages into a private directory, retaining their licence
files; do not install them or change RPM trust. Compile the fixture with the
ledger's compiler flags and private `usr/include`, `usr/include/bash`, and
`usr/include/bash/include` system include paths. Name the output
`editor-proof.so` inside that review directory, then run:

```sh
python3 tools/release/fixtures/bash_editor_journey.py \
  --review-root <private-review-directory> \
  --receipt <new-private-receipt.json>
```

The runner binds exactly the module and synthetic rc file, a read-only system
`/usr`, and private proc/dev/tmp. It exposes neither the user's home nor a
microphone, display, clipboard or external network. Receipts include exact
module, runner and installed Bash digests and cannot overwrite earlier evidence.
The fixture source carries the repository's AGPL-3.0-only licence; packaged GNU
Bash/readline licences and their exact source-package receipt are in the ledger.
No GNU source implementation was copied into the fixture.

This establishes native editing feasibility, not trusted delivery. Producer
authentication, finite leases, focus/actor binding, a stable versioned protocol,
queryable idempotent receipts, complete signal/unload handling, the existing ush
FD custody route, participant-private resources and system Undo/Redo still need
production implementation and acceptance. The fixture must not be installed or
enabled in a user shell. Arbitrary shells and TUIs remain unavailable.

## Production source candidate, 2026-10-08

`crates/runtime/ush/native/bash_editor.c` is the actual Bash/Readline producer,
not the feasibility fixture. It owns a private close-on-exec packet endpoint,
uses the existing QPE1 executing-process proof, checks its compiled Bash and
Readline versions, and publishes no buffer during startup configuration,
`read -e`, silent input or unsupported edit states. Literal replacement checks
the original incarnation, generation, revision, exact issued expiry and native
UTF-8 selection. It verifies the resulting buffer before acknowledging it.
Undo restores the original selection; Redo remains undoable. Exact duplicate
requests query/replay their receipt without applying another edit.

The admitted ush launch negotiates protocol version 1. Its descriptor bundle is
PTY first, native editor second; the original single-descriptor protocol stays
valid. The app verifies both descriptor roles. The supervisor qualifies an
OS-owned module through root-owned, non-writable ancestors, bounds its reads,
compares the selected guest Bash with the host build, and binds every producer
packet to the original executing peer and owned launch. Process exit, ancillary
rights, malformed packets and delivery uncertainty retire the channel. Neither
a configured shell nor an unsupported runtime is replaced.

Terminal retains ephemeral lease/history metadata, never the private field
buffer. Its device-only Inspect editor and secret-parameter Replace editor
selection effects use the existing resource route. The original caller is
rechecked immediately before dispatch; permission and focus changes revoke
captures. Qualified ordinary IME and dictation use the same literal request.
Native history completes through the actual acknowledgement, using the SDK's
public `LocalUndo` interface without exposing its private host operation API.
A lost or mismatched acknowledgement fails explicitly; it is never retried as
PTY input or document Undo. Ordinary IME failure has a persistent shared Banner
and localized accessible announcement. Native redisplay preserves the lease;
physical input revokes it before writing to the PTY.

The existing OS component manifest carries the module at
`/usr/lib/tilo/ush/bash-editor.so`. The native builder compiles it with the
snapshot's Bash/Readline headers and hardened flags; the builder artifact
manifest requires it. This is source packaging work only. No image was built,
module installed, compositor deployed, microphone enabled or permission granted.

`dictation-bash-editor-production.json` records the privately built production
module and actual Bash journeys. The explicit qualification executable is
`runtime-portal/examples/bash_editor_journey.rs`. It uses the production codec,
packet socket and kernel process proof in an isolated namespace with a real
controlling PTY. Seven checks cover protected startup, literal Unicode,
selection restoration, exact expiry, receipt idempotence/query, Undo/Redo/Undo,
net-zero caret changes, revocation and repeated editor generations. It also
asserts that external commands do not inherit the editor capability.

The acknowledgement worker, packet polling, monotonic deadlines and PTY-role
validation live in the platform layer (`tilo-process::editor`). Unsupported
hosts expose no editor capability through the same API. Terminal retains only
caller authority and UI receipt adapters; no portability baseline was raised.
The shared `ui::text_input` receipt facade requests no WebView capability.

Focused tests separately prove the Terminal redisplay/input boundary, menu and
read-only refusal, repeated ordinary IME input, canceled original captures,
exact acknowledgement correlation,
resource secrecy, and the generic SDK history route's original acknowledgement
and cancellation behavior. These packet-boundary fixtures are not a substitute
for the actual Bash qualification or a managed desktop journey.

Still unqualified: the full production `ush` launch with a root-owned packaged
asset, mounted standalone/embedded Terminal, in-place guest restart reconnection,
and older-device/ARM execution. The ordinary-user review cannot install a
trusted OS asset, and no installation or deployment was authorized. The browser
adapter remains with its existing owner. Mic/catalogue activation and acceptable
speech recognition latency remain independent delivery blockers.

Strict all-target Clippy passed for Terminal, ush, runtime-portal, tilo-ui
and tilo-relay. After moving the worker into the platform layer, Terminal and
tilo-process all-target Clippy and all four focused native boundary regressions
passed. The final `check --locked -p shell --tests` passed in 1m 29s. This
supersedes the earlier builder capacity refusal; no local compilation fallback
or shared cleanup was used. Portability, capability declaration, generated web
desktop and both Containerfile copy checks passed, with no policy baseline raised.

The OS image CI remains blocked on the unpublished signed RPM snapshot
`set-4cee6ac4651b25573a4c65e1dba081d96f0ecc825daa76e7cfa53c0eed58a63c`.
The existing build-rpms workflow restricts signing and snapshot publication to
main. This held source candidate cannot satisfy that gate without integration;
no merge, signing-policy change or image deployment was authorized.
