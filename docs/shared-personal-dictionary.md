# Shared local personal dictionary integration

The implementation is in `settings_store::personal_dictionary`. It is a
system service, with a host-supplied private local root and authenticated
person binding. It does not store words in an application, workspace,
collaborative document, roaming settings record, or public demo namespace.

`Service::bind(Some(person_root_key))` issues a revocable `Reader`.
`Service::bind(None)` revokes all readers; every subsequent binding also
invalidates previous readers, including a return to the same account.
The host owns the service and editing authority. Consumers receive only
the `Vocabulary` interface. A reader reloads atomic persisted state on
each operation; missing state is empty, malformed state is an error, and
errors clear personal hints. Edits compare revisions under an advisory
file lock so stale screens cannot overwrite another process's changes.
The directory and published files are private (`0700` and `0600`).

`Snapshot::hints(language)` supplies bounded exact spellings to on-device
recognition biasing. It is data for the recognizer, never replacement
instructions or an instruction prompt. `contains` protects explicit user
words from autocorrection. `completions` exposes prefix completions with
the user's spelling and casing. NFC equivalence and Unicode lowercase
comparison deduplicate matching entries; explicit edits change casing.
Language-neutral entries apply to all languages; tagged entries apply
only to the matching language or its regional variants. No fuzzy personal
replacement is performed, and ordinary words keep their input spelling.

## Existing stores and integration ownership

Current main has these separate sources:

- Writer's `widget/proofing.rs::PersonalDictionary` is an app-scoped
  personal setting (`dictionary_words`). Its `AddToDictionary` path is
  in `widget.rs`, and builds a spellcheck overlay.
- OSK's `learned.rs::LearnedTyping` is a `PersonRecord` with transitions,
  kept corrections and touch offsets. This implicit adaptation is not
  the explicit dictionary and must not be imported as explicit additions.
- `spellcheck::user_dict_path()` still names the legacy XDG
  `tilo/userdict.txt` compatibility store. App XDG roots are room-specific;
  they are not an authority for a session-wide person's vocabulary.
- App settings are delivered through
  `ui::collaboration::channel_settings` and `settings_store::app_scope`;
  `SessionSettings` separates personal and device-local records. Browser
  profile settings are installed by `tilo-browser-runtime::settings`.

The dictionary implementation owns the model, private persistence,
explicit migration API and OSK consumer. OSK's
`set_personal_vocabulary` accepts a host-issued `Arc<dyn Vocabulary>`;
`refresh_personal_vocabulary` refreshes visible chips after a service
notification. Personal chips are revalidated before insertion, so a
removed word, changed casing, revoked reader, or account switch cannot
commit a stale hint. Secret fields do not display personal completions.

The existing dictation owner owns recognition/frontend wiring and the
held PR2974. No files or release permissions of that PR are changed here.
They can consume `Vocabulary::snapshot` / `Snapshot::hints` without
introducing a second vocabulary store.

## Host delivery and editing

The native shell owns one service beside its settings copy, outside room
roots. `personal_dictionary::host::Host` binds it to the verified host root
identity, resolved through the same active selector as first boot and the
launcher. The existing AppSettings capability projects an ephemeral read
view only for an admitted local person worker whose certified root matches
that live binding. Agents, visitors, foreign/capped identities and remote
workspace collaborators receive no vocabulary. Admission is revalidated on
every read and edit. No account or sync permissions are added.

The projection and edit command use reserved keys in the app's existing
own device-local settings capability; neither is persisted in settings or
roaming records. SDK settings reads exclude the virtual metadata. Ordinary text fields follow
the same read view for spelling/autocorrection and route explicit dictionary
additions through it, with no app-local dictionary file writes. The small
consumer interface is `ui::personal_dictionary::reader()`, implementing the
same `Vocabulary` trait. Account replacement revokes old readers, even when
returning to the same account. Create a new reader after switching; errors
clear hints. OSK refreshes and rebinds its reader on account changes.

Settings → Mouse & Keyboard presents shared TextInput/Button controls for
add, edit, remove and reset, with language tags and exact entered casing.
The controls invoke typed semantic Settings effects. Form contents clear
on account changes. Writer's explicit addition action writes this same
service and its spelling overlay follows shared words; spelling caches
invalidate when words change.

The legacy global `userdict.txt` and Writer's previous explicit personal
words import once. Host migration removes the old Writer words from its personal settings
record and excludes them from settings views, including during recovery.
Unscoped legacy stores migrate only to the verified original `primary.json`
profile, never an alternate active account. Missing identity provenance leaves
old words unexported and recoverable. An ownership marker additionally prevents
another account claiming that source; reset cannot revive imported words. The host reads only its known profile's bounded regular legacy
file, with no symlink following. OSK learned transitions stay separate as
implicit adaptation and are never presented as explicit additions.

The dictation owner retains ownership of actual recognition/frontend
integration and PR2974's hold. This PR supplies shared local biasing data
and cross-consumer delivery tests; actual recognizer qualification belongs
to that owner, with no change to the held PR's release permissions.

## Focused verification

Run through a remote Cargo drop-in configured as `$CARGO`, keeping compilation
off the interactive device:

```bash
$CARGO test --locked -p settings-store personal_dictionary --lib
$CARGO test --locked -p osk personal_vocabulary_tests --lib
$CARGO test --locked -p tilo-ui --lib collaboration::channel_settings::tests
$CARGO test --locked -p tilo-action-broker --lib app_settings_tests
$CARGO test --locked -p word-processor --test it framework_host::the_persons_own_words_are_spelled_right_in_every_window -- --exact
$CARGO check --locked -p shell --tests -p settings -p osk -p word-processor --all-targets
```

Service tests exercise real disk persistence, edit/remove/reset/restart,
case-preserving completions, Unicode, language selection, duplicate words,
stale revisions, independently opened services, corrupt files, private modes,
one-time import and account revocation. OSK tests exercise actual composing,
completion activation, ordinary-word preservation, explicit autocorrect
protection and stale-chip rejection alongside the shared biasing reader.
The biasing reader tests do not constitute actual recognizer qualification.

## Editor screenshot

[Mounted personal dictionary settings](https://code.tilo.so/api/packages/tilo/generic/source-evidence/529c35c827cda21dbcc538571ef3f6e3a9f41fee91f4a0f55783c5685f541041/personal-dictionary-live.png)
is an offscreen capture of the mounted production Settings app after its real
Add action and casing/language edit persisted `TiLo` (`nl`) through the shared
host service. Add, Save, Remove and Remove all words are live action controls.
The same test verifies restart, remove, reset and identity revocation.
SHA-256: `529c35c827cda21dbcc538571ef3f6e3a9f41fee91f4a0f55783c5685f541041`.
Regenerate on the remote builder with `TILO_DICTIONARY_LIVE_SCREENSHOT` set for
`panes::mouse_keyboard::tests::mounted_dictionary_actions_add_edit_remove_reset_and_survive_restart`.

[Unicode editor fixture](https://code.tilo.so/api/packages/tilo/generic/source-evidence/ff0d6e7640a6c110aef0229904ac32f24bb478bd945dd7b95ef4952ffb48d25a/personal-dictionary-settings.png)
also shows the production controls with `Tilo`, `Élodie` (`fr`) and `東京` (`ja`)
from an isolated service profile. That rendering fixture has no action dispatcher,
so its mutation buttons are disabled; the mounted capture above supplies live
interaction evidence.
