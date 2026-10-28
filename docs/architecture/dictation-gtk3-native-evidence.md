# GTK 3 native dictation qualification

The production compositor at `b632d82ddb4b8e4402932f61715d289e8ba0c9fc`
passes the complete recorded-speech editing journey in a standard GtkSourceView
4.8.4 field on GTK 3.24.52. No GTK 3 input-method or undo adapter was required.
The signed Fedora runtime was extracted privately for qualification; it was not
installed in Tilo or the user's desktop.

The initial compatibility failure was in clipboard observation, before speech
recognition. The first GTK copy completed, but subsequent copies stalled after
Tilo requested `SAVE_TARGETS` as a payload. GTK advertises this clipboard-manager
operation alongside its data formats. Tilo now excludes that operation from its
bounded asynchronous data pulls. Other formats and selection ownership guards
remain in place. A real Wayland client regression fails on the unwanted send
event before the fix and passes afterward, including a replacement selection.
The existing native text/PNG clipboard journey also passes.

The fresh GtkSourceView run verifies the original buffer and selected range,
nonempty streamed preview, selected-range replacement, one native undo/redo,
unchanged clipboard, canceled speech, repeated sessions and focus-away/back
retirement. Separate native password and PIN fields refuse dictation before
capture consent, recorder or worker startup. The ordinary field remains eligible
after those refusals. The shell's complete test targets compile.

With the same public German FLEURS recording and approved Small Q5_1 model, the
first nonempty preview appeared at **9.70 seconds**, and insertion completed at
**27.91 seconds** from capture start on two CPU cores. Normalized scoring found
**0 errors in 18 words**. Casefolded scoring is not a capitalization benchmark,
and these timings do not establish fluent, low-latency streaming. This qualifies
the tested GtkSourceView field, not every GTK 3 application or custom editor.

All four CI workflows passed on predecessor `b57ad8be`; that checkpoint does
not qualify the newer clipboard change. Its exact-head CI must pass separately.

The [machine-readable ledger](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/architecture/dictation-gtk3-native-evidence.json) retains source
and binary identities, signed-package provenance, failed diagnostics, limits,
results and canonical archive references. Logs preserve escape characters and
line endings as JSON line arrays. Concatenating `lines` reconstructs the archived
stdout. The owned remote build root is replaced with `${BUILD_ROOT}` and
the developer home with `/var/home/you`; original byte digests remain recorded.
Byte-identical artifacts reference their existing canonical file.

All five exact owned native-run containers were verified absent. Recorder,
worker, ephemeral model witness and namespace-keeper cleanup is asserted in the
run logs. No physical microphone, persistent grant, production model activation
or deployment was involved.

The protocol diagnosis follows GTK's own
[selection queue](https://github.com/GNOME/gtk/blob/3.24.52/gdk/wayland/gdkselection-wayland.c),
[clipboard-manager operation](https://github.com/GNOME/gtk/blob/3.24.52/gtk/gtkclipboard.c)
and [Wayland input context](https://github.com/GNOME/gtk/blob/3.24.52/modules/input/imwayland.c).
An inactive text-input object before preparation is expected when no input
method owns the seat. No focus or late-binding patch was made on that premise.
