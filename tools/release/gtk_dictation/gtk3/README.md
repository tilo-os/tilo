# GTK3 stock-entry history source candidate

This is an **uncompiled, unqualified, opt-in source candidate**, not an installed
runtime or an existing-history adapter. It leaves GtkEntry and its GtkEntryBuffer
in place. No UI-core or application toolkit refactor is involved.

## Exact sources and licence

`source-lock.json` pins the GTK 3.24.52 GNOME source archive, its upstream commit,
the exact GTK 4.22.5 history implementation and private headers, and the patch.
The archive's `gtk/gtkentry.c` matches the pinned upstream file byte for byte.
This is upstream GTK3, not a reproduction of Fedora's `3.24.52-1.fc44` RPM:
Fedora's spec and downstream changes are not included.

Upstream GtkEntry and GtkTextHistory are LGPL-2.0-or-later. Keep their original
copyright/licence headers and the source archive's COPYING when building or
redistributing a modified GTK library. `gtkistringprivate.h` is copied unchanged
from the same GTK4 source tree and remains under that tree's LGPL licence; it
has no individual copyright header. The patch changes GTK library code under
LGPL-2.0-or-later. The original preparation tool and public-data test subject are
AGPL-3.0-only. No upstream implementation is relicensed as repository AGPL.

## Native implementation boundary

The patch adds GTK's own history object inside GtkEntry's private implementation.
It observes actual stock buffer insert/delete results through the existing native
GtkEditable path. Selection replacement inside `gtk_entry_enter_text` is one
history group, separated from adjacent typing with native history barriers.
Undo/Redo use the same GtkEditable path and restore the original selection.
History is limited to 100 levels. Programmatic full-value replacement, buffer replacement, unowned buffer edits and
sensitive-purpose/visibility transitions clear retained history.

An owner must explicitly register a nonempty `history-owner` string (at most
128 bytes) and set the native GObject `enable-undo` property. These properties
are application-owned configuration, not identity authentication or a permission
grant. No focus event or accessibility query registers an owner. Undo is false
by default; only exact GtkEntry with exact GtkEntryBuffer is eligible.
Derived widgets/buffers keep their own history and are not automatically adapted.
`history-registered` reports the supported owner contract; `history-available`
and `can-undo`/`can-redo` enforce native editable, visible, sensitive, focused,
non-PASSWORD/non-PIN admission. Attached custom Undo/Redo handlers are refused.
The existing GtkEntryAccessible AtkAction retains activate index 0 and exposes
Undo index 1 and Redo index 2 only for a supported registered owner. Invocation
rechecks availability and possible history, then emits the same native signal.
ATK attributes `tilo-history-registered`, `tilo-history-available`, `tilo-can-undo`
and `tilo-can-redo` report booleans; they disclose no text or owner identifier.
History notifications use the existing ATK visible-data-changed signal. Native `undo`/`redo` action signals
have keyboard bindings; these are ordinary entry history, not dictation history.
The candidate **does not** add a Tilo semantic bridge or permission bypass.

## Fetchless preparation

Acquire the exact public sources named in the lock through the existing approved
source-review path. This helper itself has no network, package, compiler,
installation or activation operation:

```sh
python3 tools/release/gtk_dictation/gtk3/prepare.py \
  --gtk3-archive /private/sources/gtk-3.24.52.tar.xz \
  --history-source /private/sources/gtk-4.22.5 \
  --out /private/review/gtk3-history-prepared
```

The history source directory must contain all three pinned `gtk/` files. The
output must not exist. Preparation verifies hashes before creating it, safely
extracts the exact source archive, verifies the touched base files, copies the
history files, checks/applies the patch with git, and records resulting hashes.
Failed output is private diagnostic evidence; use a fresh output for another run.

In the already approved remote compatibility build environment, build this
prepared GTK source using its existing Meson project and **private** build/prefix
directories. Inspect `meson_options.txt` before selecting existing Wayland-only
development inputs. Do not install into the host, add dependencies implicitly,
or use a native shared target. Keep the source lock and full build logs with the
result. A concrete build command depends on those observed environment inputs;
preparation never treats a missing compiler/library as success.

`entry_subject.c` is a standard GtkEntry client that sets the native opt-in,
initialises the public text `alpha beta gamma`, and selects `beta`. Build/link it
against the exact private candidate library closure. It fails without the new
property. It adds no input method or private Undo implementation.

## Required validation and remaining limits

1. First compile the modified toolkit and subject, with fatal GLib warnings.
   Verify native library loading, properties, bindings, and no class-registration
   conflict in every intended application closure.
2. Reuse the strict `fixtures/gtk3_dictation_20261010/single-line-r7` paced recorded
   journey, replacing only the stock subject/runtime. Keep selection replacement,
   one Undo/Redo, cancel/restart, focus retirement and protected-field assertions.
   The old r7 Undo failure stays failed; later steps there were not run.
3. Add ordinary typing before dictation, repeated commits, reversed Unicode
   selection, maximum-length filtering, direct programmatic buffer edits, buffer
   replacement, two entries sharing a buffer, and visibility/PASSWORD/PIN changes.
4. Exercise custom edit handlers, nested/reentrant mutations, intercepted Undo
   signals, and destruction during commit. No generic atomicity guarantee for
   these callbacks has been proved. Stock-only type checks do not identify custom
   handlers attached to a stock instance. The registration contract now exists, but
   callback atomicity and runtime signal collisions still require qualification.
5. Validate public native action/status accessibility in the intended runtime;
   the new ATK actions and properties require real runtime observation and do
   not by themselves prove the full system semantic
   Undo journey. No synthetic-key-only result establishes that guarantee.

A guest OS library change does not cover Flatpak-owned GTK runtimes. Deliver only
inside an exactly matched app/runtime closure after qualification. The existing
legacy guest workflow signs/pins guest images, but this candidate adds no workflow,
publication, global environment, new trust root or OS rootfs toolkit exception.

The private native build driver is `private_build.py`. It requires a fresh
preparation receipt including a whole-source snapshot, and an extracted,
signed development closure with an exact file/symlink snapshot and no unresolved
requirements. Its default is preflight only; `--execute` explicitly performs the
bounded build. It uses the existing pinned compatibility image with no pull,
network or package installation, two CPUs, 3 GiB and no swap. The source and
extracted dependencies are read-only; only the fresh private output is writable.
A successful compile is not the single-line speech/Undo or accessibility gate.
Neither runtime installation nor activation is part of this driver.
