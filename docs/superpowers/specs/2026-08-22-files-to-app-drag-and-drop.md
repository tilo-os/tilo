# Files → app drag-and-drop (Wayland)

**Date:** 2026-08-22
**Record type:** Historical design record. Current behavior is defined by the compositor, winit integration, and end-to-end tests.

## Purpose

A file dragged from the built-in Files surface to a sandboxed native app is a
compositor-mediated Wayland data-device transfer. It is not an in-process Files
move and it is not a new filesystem grant. The resulting client event lets a
format-aware app, such as the word processor, place a dropped image at the drop
location.

## Transfer contract

When an active Files drag crosses from hosted chrome into a foreign client tile,
the compositor becomes the Wayland DnD source. It offers `text/uri-list` and, for
a single file where appropriate, the file's concrete MIME type. A client chooses
an accepted offer; pointer motion during the drag continues to establish the
client-local drop position; release completes or cancels the transfer.

For URI offers, the receiver obtains decoded `file:` paths. For byte offers, the
client receives a temporary runtime-directory file and is responsible for removing
that spool file after a successful read. The protocol delivers no other URI
schemes as files.

The hosted Files UI must be told when the external transfer finishes so it does
not also perform its internal folder or favourites drop. Its drag feedback remains
visible throughout the hand-off.

## Security model

The compositor serves only the file the user explicitly dragged. This is the same
user-mediated authority as a file chooser and must not widen the target app's
workspace, filesystem, or persistent portal grants. Passing bytes is necessary
when a confined client cannot independently resolve the source path.

Client-to-client DnD, drops into Files, workspace drops, and in-process tab drags
are separate mechanisms with separate authorization and interaction contracts.

## Receiver behavior

The Wayland backend maps data-device lifecycle events to hover, hover-cancel, and
drop events. MIME selection prefers URI lists and otherwise uses a supported file
MIME type. Drop data is read asynchronously so the event loop stays responsive;
the offer is finished only after the transfer has completed.

Apps must treat hover as optional feedback and treat a drop as an ordinary local
file input. An image-capable editor inserts a valid image at the pointer position;
other apps may decline a MIME type without changing their permissions.

## Verification invariants

Tests cover source-offer metadata and bytes, cancellation without corrupting the
Files drag state, URI parsing and MIME preference, and a cross-process headless
session that proves a dragged image reaches the word processor. Existing Files
drag-to-folder behavior remains a regression boundary. The end-to-end test waits
for the target's offer acceptance before release, because an unaccepted Wayland
offer is correctly refused.
