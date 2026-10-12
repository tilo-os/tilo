# Public source distributions for native capture qualification

These are **inputs**, not a qualified package, a login session, or a native-run
receipt. Native qualification still has to launch the real producer and consumers
in the protected image. Nothing here substitutes for those mandatory steps.

The admitted source distribution is Linux **x86_64**, Electron **44.4.5**, and
Slack **4.52.178**, Snap revision **263**. No other architecture or version is
silently admitted. This is the source fixture distribution, not a declaration
that release targets on other architectures have passed native qualification.

`service-source-subjects.json` contains the full measured ledger: both archive
subjects and every file's relative name, size, mode and SHA-256, including the
repository fixture app. It contains no builder path. Its canonical SHA-256 is
pinned in `service-source-assets.py`, independently of the archive SHA-256 pins.
Only `source_root` and `source_path` locators are excluded from subject identity;
unknown fields, altered endpoints, substituted archives and partial trees fail.
Legacy schema-1 operator manifests remain accepted with the same distribution
identity, after relocating their locators. The public flags must remain false.

## Supply bytes on this builder

Copy the pinned archives and **complete** extracted trees to a builder-owned,
non-group/world-writable directory, without symlinks or special files. Preserve
file modes, including executables. Neither root ownership nor an old builder's
path is necessary for these public inputs. Root ownership remains necessary for
the qualifier's newly owned candidate staging area.

The pinned public archive URLs are:

- `https://github.com/electron/electron/releases/download/v44.4.5/electron-v44.4.5-linux-x64.zip`
- `https://api.snapcraft.io/api/v1/snaps/download/JUJH91Ved74jd4ZgJCpzMBtYbPOzTlsD_263.snap`

Use those exact subjects, not a moving latest URL or another Slack package.
Electron's tree is the complete ZIP contents. Slack's tree is the complete
`usr/lib/slack` subtree of that Snap (extract with `unsquashfs`); keep every
`resources/app.asar.unpacked` file. The Snap itself is also mandatory and measured
independently. Do not fabricate a substitute package or reduce the extracted
file set to just the binary/ASAR. No network request is needed by the admission
helper.

Generate an operator manifest from the checked-in subject ledger. `$ASSETS` is
your chosen absolute builder-local directory, for example a private directory
under `/var/lib` or `/tmp`, not an inherited worktree path:

```sh
python3 apps/imager/packaging/service-source-assets.py \
  --electron-archive "$ASSETS/electron-v44.4.5-linux-x64.zip" \
  --electron-root "$ASSETS/electron" \
  --slack-archive "$ASSETS/slack-263.snap" \
  --slack-root "$ASSETS/slack/usr/lib/slack" \
  > "$ASSETS/source-assets.json"
export TILO_CAPTURE_SOURCE_ASSETS="$ASSETS/source-assets.json"
```

The default fixture app is the checkout's
`os/scripts/service-session-source-fixture/app`; `--app-root` can locate an exact
copy elsewhere. The helper measures all archives/files before emitting JSON. It
does not chmod inputs, launch applications, create sessions, or assert provider
authentication. Relocated inputs are measured again during root-owned staging.

Pass this manifest through the existing release preparation/qualification
pipeline (`TILO_CAPTURE_SOURCE_ASSETS` or qualifier `--source-assets`). Read-only
binds use the manifest's actual local locators; the qualifier protects runtime,
interpreter and candidate namespaces. Its mount policy accepts inputs below
`/tmp`, `/var` or `/home`, not `/dev/shm`. Store the manifest and public inputs in
one of those admitted namespaces for a real qualification run.

## Audit basis

The frozen ledger was reconstructed from the existing public asset manifest,
not its saved execution claims. Both actual archive hashes and every current
extracted file size/mode/hash were independently remeasured. Electron's complete
ZIP entries were also compared to the ledger; Slack's version was read directly
from the measured `resources/app.asar` package entry. The fixture app hashes were
checked against this repository. Local archive/extraction paths do not appear in
the new subject ledger. This audit supplies no native or provider-authentication
qualification evidence.
