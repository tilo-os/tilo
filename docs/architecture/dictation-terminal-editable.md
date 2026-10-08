# Terminal editable-resource contract

This is an integration contract, not an implemented terminal dictation backend.
The source inspection at `1196d1340bfef9c6b72305e4b1677f108add04bc` found no
cooperative command-line editor producer in Terminal or ush. Implementing that
producer and its reversible transaction route is engineering work; microphone
consent cannot supply them.

## Existing boundary

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
