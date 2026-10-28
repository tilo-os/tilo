# Native Qt dictation acceptance

The real FeatherPad editor and Qt's standard `QPlainTextEdit` and `QTextEdit`
pass the full recorded-speech
journey without a GTK adapter or clipboard/key insertion fallback. Native
`QLineEdit` now passes the combined speech, editing and protected-field journey;
an editable `QComboBox` passes through its actual inner `QLineEdit` too. This
broadens toolkit coverage; it does not qualify every third-party application,
X11, Flatpak, physical microphone, language or device.

The production compositor was remotely built at
`8d8240ba90f2e2ff7e485e2e9c75a592c5f1972a`; its message-only successor
`026841a1d855604e495f39f361c642c819d12d91` has the identical tree
`3f4dca9b56345c98bc0514d03d1bc39db1518a38`. The sealed speech worker and ephemeral
model witness remain byte-identical to their previous controls. The ledger
pins the newly built compositor, each native fixture, model and audio separately.
The signed private Fedora Qt runtime is 6.11.2-3.fc44. Its headers and licences
remain in the owned test directory; nothing is installed or added to the OS image.
The only missing loader dependencies were supplied as exact signed private
libb2 and double-conversion packages. Early preparation failures remain archived.

The native fixtures use standard Qt Widgets, with compositor-owned window chrome.
Their Wayland input context selects `zwp_text_input_v3`, which Qt explicitly
supports in its
[protocol selection](https://github.com/qt/qtbase/blob/v6.11.2/src/plugins/platforms/wayland/qwaylanddisplay.cpp#L534-L565).
Qt groups selected-text removal and committed text in one native edit block
([text control](https://github.com/qt/qtbase/blob/v6.11.2/src/widgets/widgets/qwidgettextcontrol.cpp#L1903-L1938)).
The real keyboard Undo/Redo assertions verify that behavior through the
compositor's existing control socket and clipboard observation.

| Native field | Recorded insertion | Whole scoped run | Result |
| --- | --- | --- | --- |
| QPlainTextEdit | 27.253 s from recording start | 45.885 s | Full journey passes |
| QTextEdit | 27.159 s from recording start | 47.321 s | Full journey passes |
| QLineEdit | 27.962 s from recording start | Combined run fails later privacy premise | Speech/edit/session assertions pass |
| FeatherPad 1.6.2 | 28.072 s from recording start | 47.147 s | Full real-app journey passes |

All four insert the fixed public 17.94-second German FLEURS recording with zero
normalized word errors out of 18 reference words. This is one recording, not a
general language benchmark. The full editor journeys preserve surrounding text
and clipboard, restore the selected original word with ONE Ctrl+Z, restore the
result with ONE Redo, cancel active speech without editing, restart a worker,
and permanently retire across two presented native windows and focus back.
Their scoped peaks and controls are retained in the ledger. Each runs with two
CPUs, 3 GiB, zero swap and 256 tasks, no network or hardware audio, and a private
PipeWire/WirePlumber virtual source. Workers, recorders, ephemeral model witnesses,
namespace keepers and owned containers are reaped.

The actual FeatherPad app is the signed Fedora 1.6.2-3.fc44 package, with the
signed QtSvg 6.11.2-1 dependency privately extracted into the same runtime.
Its [upstream standalone invocation](https://github.com/tsujan/FeatherPad/blob/V1.6.2/featherpad/main.cpp#L45-L75)
opens two independent real editor windows. The first presentation attempt fails
before recognition because the display name's casing was incorrectly assumed to
be the Wayland app ID. The receipt observes lowercase `featherpad`; the corrected
journey passes the unchanged strict editing/session assertions. The absent stale
QtSvg package URL and the successful current package receipt remain separate.

The first single-line driver fails its exact original-selection assertion before
recognition. A retry uses the native single-line Home key and retains the same
assertion. That retry proves all speech/edit/session assertions, then fails the
separate requirement that protected fields must activate text input. This run
remains failed. A separate privacy journey observes actual native field focus,
public hint integers, and matching keyboard/routed/text-input surfaces. Password,
PIN-style and sensitive-data fields offer no capture consent and start no recorder
or worker. Their native `ImEnabled` query is true while the compositor's observed
text-input-active flag is false. No causal claim about that inactive state is
needed for refusal acceptance. The normal entry first starts and cancels a real
worker/recorder to establish the working control. The privacy run performs no
speech recognition and reads no password content. Qt's
[echo-mode implementation](https://github.com/qt/qtbase/blob/v6.11.2/src/widgets/widgets/qlineedit.cpp#L510-L522)
sets hidden/sensitive hints for masked fields; the exact observed values are in
the ledger.

The raw multiline manifests inherited a GNOME editor description and GTK renderer
label. These two metadata errors remain visible with explicit ledger errata;
the actual Qt client identity and presented application ID identify the subject.
No failed run is relabeled as passed. Complete raw output is preserved losslessly
as escaped lines alongside its original and portable hashes.

PR #4014 was initially parked as WIP/draft with `hold` while source-delivery
authorization was being checked. The parent provenance audit subsequently
withdrew that temporary restriction and confirmed the user's tested-merge
authorization. Source integration must pass exact-head CI and the protected
repository process. Physical microphone access, new persistent permissions,
paid APIs, production signed-model admission and deployment remain distinct
from these already authorized public recorded-audio tests and source delivery.
The exact bookmark identity of "phonnon 2" remains unverified.

The subsequent current-source qualification at
`cb51d4b11a9ca80e16abd775162677b2bdb4da04` reproduces the original combined
single-line failure before correcting its premise. Qt's
[platform acceptance code](https://github.com/qt/qtbase/blob/v6.11.2/src/gui/kernel/qinputmethod.cpp#L329-L351)
separates a widget's `ImEnabled` query from platform acceptance and can reject
hidden input. Native field focus is therefore not equivalent to the compositor's
text-input-active flag. The corrected journey requires actual `FocusIn`, the
exact protected hint integer, and matching keyboard, routed keyboard and
text-input surfaces. It preserves the no-consent, no-recorder and no-worker
assertions and adds a working ordinary-field control after all protected
refusals. It does not claim a causal explanation for each inactive protected
field. The historical failure and its fresh reproduction remain failed archives.

| Current-source native field | Recorded insertion | Whole scoped run | Result |
| --- | --- | --- | --- |
| QLineEdit, corrected combined journey | 26.137 s | 49.709 s | Full journey passes |
| Editable QComboBox, actual QLineEdit child | 27.472 s | 51.939 s | Full composite journey passes |

Both use the same pinned public German recording and Small Q5_1 model, with zero
normalized word errors out of 18 reference words. The source-pinned production
compositor, worker and ephemeral witness were remotely rebuilt; exact binary
identities are in each new manifest. The signed Qt runtime was privately restored
after builder eviction, without installing or activating anything. The observed
scoped memory peaks are 1,069,142,016 and 1,333,760,000 bytes respectively under
the same two-CPU, 3-GiB, zero-swap and 256-task controls. The full editing, cancel,
repeat and two-window retirement assertions still run before the strengthened
privacy assertions. All three exact owned qualification containers are confirmed
absent, and each full log verifies capture, witness and namespace cleanup.
These receipts qualify a standard composite control; they do not qualify every
custom editor, physical microphone, language or device.

[Ledger](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/docs/architecture/dictation-qt-native-evidence.json) ·
[Full historical fixtures](https://code.tilo.so/tilo/tilo/src/commit/0ebed9cae919c6f3e2a57565bde34beac0325ec5/tools/release/fixtures/qt_dictation_20261010/README.md)
