# GTK native selected-range dictation

The actual GNOME Text Editor journey now passes ONE native Undo and ONE Redo
with an opt-in client transaction module. Stock GTK still fails the same strict
requirement. This component is not installed or selected by the normal image,
and does not establish universal editable-field coverage.

GTK 4.22.5 intentionally ends one buffer action after deleting a multi-character
selection and starts another before inserting the input-method commit
([GTK source](https://github.com/GNOME/gtk/blob/4.22.5/gtk/gtktextview.c#L8263-L8281)).
The compositor already emits one literal commit, with no clipboard or key
fallback. The module forwards that commit through GTK's existing Wayland
context, with an outer native GtkTextBuffer action for GtkTextView clients.
GTK's nested action accounting keeps the outer operation together
([buffer source](https://github.com/GNOME/gtk/blob/4.22.5/gtk/gtktextbuffer.c)).
The private OSK class slot requires exact runtime compatibility review.

The hardware-disabled test uses the unchanged production compositor and sealed
speech worker at `920cc7541449bdf53b557acf4939682bb1bdc47b`, GTK 4.22.5,
GtkSourceView 5.20.0 and GNOME Text Editor 50.1, with quantized Whisper Small.
All recovered compositor/worker/witness binaries are byte-identical to the
previous controls. The fixed public German recording has zero normalized word
errors out of 18 reference words. Insertion takes 27.963 seconds from the start
of the 17.94-second recording; the complete inner journey takes 39.700 seconds.
Total sampled scope memory peaks at 965,001,216 bytes under two CPUs, 3 GiB,
zero swap and 256 tasks. This includes software graphics, debug executables and
file cache; it is not a production footprint or general language benchmark.

The actual editor preserves the surrounding text and clipboard, restores the
original selected word with ONE Ctrl+Z, reapplies the dictated replacement with
ONE Ctrl+Shift+Z, cancels active speech without editing, starts a fresh worker,
and retires permanently across two presented editor windows and focus back.
Worker, recorder, ephemeral model witness and namespace keeper cleanup all pass.

A separate standard-widget field journey observes native focus and ordinary
single-line/multiline typing, selected-range keyboard Undo/Redo, and password/PIN
refusal before recorder or worker startup. GTK's PRIVATE hint requests no
personalized-data updates, rather than secret-field refusal
([GTK definition](https://docs.gtk.org/gtk4/flags.InputHints.html)). The earlier
stricter PRIVATE-refusal assertion remains failed in its own complete receipt.
The correctly scoped additional case verifies one-session consent/cancellation
and an unchanged nonempty original field. It does not measure recognition or
personalized-data learning for PRIVATE fields. Production dictation already
avoids automatic transcript/audio history.

The [opt-in component](../../tools/release/gtk_dictation/README.md) and builder
fetch nothing, install nothing and preserve native input purpose/hints. Host
launches now retain an explicitly configured GTK_PATH alongside GTK_IM_MODULE;
they do not acquire sandbox or collaboration authority. Compatible legacy app
packaging owns its toolkit. The native OS toolkit exclusion remains intact.

The disappearing remote artifacts were recovered through normal serialized
rcargo builds. Existing watermark/LRU collection can remove idle worktrees;
the exact deleting actor remains unconfirmed by the scoped journal. Subsequent
qualification held only the existing owned worktree lock through prep/test/full
export, with bounded lifetime and no runner reservation or global policy change.

[The ledger](dictation-gtk-client-transaction.json) retains exact source/model/
audio/module/package identities, controls and original/portable raw hashes.
[Historical drivers and full outputs](../../tools/release/fixtures/gtk_dictation_20261010/README.md)
remain distinct from the earlier stock-GTK undo failure and safety-only pass.
The repository module changes only the historical source's SPDX comment to
match the repository licence; its repository builder corroborates identical complete executable bytes
(SHA-256 `5853c6c79c37bddadad65c3c89696c72ff2075ac7314014537cd50a3d48fd4d7`).

Production signed-model admission, compatible legacy-runtime packaging/activation,
GTK3/Qt/X11/Flatpak coverage, physical microphones, ARM/older devices and broader
editor/language acceptance remain unqualified. No microphone permission, persistent
access grant, global input-method selection, image deployment or PR metadata change
is part of this increment.
