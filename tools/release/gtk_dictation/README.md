# Opt-in GTK 4 native commit transaction component

`atomic_im.c` is an experimental compatibility module for **GTK 4.22.5 on
Wayland**. It delegates GTK's existing Wayland input context. For a GtkTextView
or GtkSourceView, an outer native buffer user action groups the one forwarded
commit, so replacing a selected range takes one Undo and one Redo. Other GTK
controls retain their native commit behavior. Input purpose and hints are bound
unchanged to the delegate; no clipboard or synthetic-key insertion is added.

The class's private OSK vfunc slot makes the exact runtime pin deliberate. This
is not a GTK3, Qt, X11 or arbitrary-version module. The build helper refuses
different GTK headers or runtime versions. Runtime packaging must preserve that
pin before selecting the module; the library itself is not a runtime admission
service. Do not select it for an unmatched app runtime.

Build only in the approved remote compatibility environment, with existing
GTK 4.22.5 development inputs:

```sh
python3 tools/release/build_gtk_dictation_adapter.py --out target/gtk-atomic-review
```

For already verified development RPMs extracted privately rather than installed,
`--headers-root` names their combined root. Retain the signing, digest, package
version and licence receipts with the build. The helper fetches nothing and
creates a fresh output containing `immodules/libtilo-atomic.so` and a digest
manifest. It does not install or activate anything. GIO can scan the directory
without a cache; the recorded native journeys exercised that path.

For a disposable compatible **Wayland test client only**, set
`GTK_IM_MODULE=tilo-atomic` and `GTK_PATH` to that output root. The module is
registered at priority zero and does not replace the default input method.
Do not set these globally or add GTK/Qt packages to the native OS rootfs.
Compatible legacy app packaging owns its toolkit and module tree. Host app
launches preserve an explicitly configured GTK module and its discovery path;
they remain unconfined host apps and receive no Tilo authority variables.

`privacy_client.c` is a qualification fixture using standard GtkEntry and
GtkTextView controls, not a shipped app. Its public, nonempty PRIVATE-hint fixture
makes actual native Copy an observable buffer read; copying an empty GTK entry
intentionally leaves the previous clipboard untouched. PASSWORD/PIN refuse
recording. GTK PRIVATE means no updates to personalized data, not secret-field
refusal. The separate no-learning contract remains the OS speech service's
responsibility; the field fixture checks consent, cancellation and ordinary input.

See [the evidence and limits](../../../docs/architecture/dictation-gtk-client-transaction.md).
No model, audio, development headers or native library is distributed here.
These original sources use the repository's AGPL-3.0-only licence.
