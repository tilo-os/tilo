# Demo video — what is left

Status read from the tree and from live runs, not from memory. Updated
2026-08-22 evening.

## Blocking the video

**1. Beat 7 — two word processors must converge.** *In progress.*

Presence crosses between machines and paints correctly. Documents do not:
alice's app logs `broadcast doc delta 1135B`, bob's app logs no
`recv CRDTIncremental` and no `recv CRDTUpdate`, ever. Both directions.

`app-collab-proof.py` passes and looks like it proves the opposite — but it
uses synthetic JSON clients, not the packaged app, which is why this went
unnoticed. Do not trust it for this question.

The delivery decision is `CrdtRoute::delivers_to`
(`shell/src/shell/collab_broker.rs:402`). Leading hypothesis: capnp
subscribers recently gained an app id, so routes that used to match by
falling through the cluster-scoped early return now have to match app id
*and* record id exactly, and something disagrees. **Being measured, not
argued** — the values at the decision point settle it.

A legitimate outcome: two people each opening a *blank* document are opening
two different records and should not converge. That would change the
storyboard (both open the *same* document) rather than the code.

**2. Record the nine beats end to end.** *Scaffolded, not yet run clean.*

`scripts/yc-demo.py --record` has all nine beats written. Needs a full run on
the repaired machine, then the output watched frame by frame.

**3. Watch the result.** Not optional, and not the same as reading assertions.
A peer's caret over an empty page passes every byte-level check and is exactly
the failure beat 7 exists to fix. Two earlier cuts shipped with a fake peer in
them because nobody looked.

## Blocking the opening shot

**4. OS boot footage.** *In progress.*

No qcow2 exists yet. `os/dev/record-boot.py` was written today. Order is
cheapest-first: find an existing image, else dispatch CI (`build-image.yml`,
`workflow_dispatch`), else build locally — and image builds belong in CI by
standing instruction.

Known constraint: QEMU `screendump` cannot read a GL surface
(`os/dev/vm.sh:38-42`), so `--gl` and screendump capture may be mutually
exclusive. Establish which combination works rather than assuming.

## Not blocking, but wanted

**5. Merge to main.** *Gate armed.* 149 commits. `cargo check -p shell --tests`
runs the moment cargo frees, then main fast-forwards. CI is **inert until this
lands** — Forgejo schedules only fire from the default branch — and main still
carries fabricated collaborators this branch removes.

**6. Time a qpkg install.** Beat 5 currently cannot claim "fast" because nobody
has measured it. One clean session, one install, one number.

## Decided, closed

- **Beat 8 films the file chooser, not a drag.** Investigated: the launched
  Files is a sandboxed Wayland client and neither it nor the toolkit opens a
  `wl_data_source`, so there is no client drag for the compositor to promote.
  The chooser is the better beat regardless — a drag shows a file moving, the
  chooser shows the sandbox: the app asks, the OS chooses, the app is handed
  bytes rather than a path it has no right to read.
- **Beat 9 is `qos-agent`** — joins the document as a participant with no
  window, edits through the word processor's own exported actions, appears in
  the history strip and as a caret in the others' windows. Vocabulary is
  scripted, and the honest framing is the stronger one.
- **The fake "Ada" peer is deleted**, not gated. Any footage from before
  2026-08-22 contains a collaborator who does not exist.
- **First boot is honest** — one empty workspace, no invented people,
  workspaces, tabs or documents. Verified with before/after screenshots.

## Things that must not be said

- "install is fast" — not measured
- anything about beat 7 until its proof is green
- the peer cursor in old footage is not a person
