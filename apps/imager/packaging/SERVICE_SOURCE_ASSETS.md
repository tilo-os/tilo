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
your chosen absolute builder-local directory on durable host storage, for example
`/srv/tilo/tasks/installer-public-source/target/capture-public-assets`, not a
container layer, `/tmp` or `/dev/shm`:

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

## Repeatable recovery after a builder refresh

The generator source is already checked in at
`os/scripts/service-session-source-fixture/app/main.cjs`. Its 7,730 bytes have
SHA-256 `c138828e660c41f53bad6bc3eff0c372b8950588b209ff2cbce583916dbb62e3`;
`package.json` has 151 bytes and SHA-256
`c460efc2fcb42a233ed251afc8846c94f59c5d6c63ce150975ee2eac2962f886`.
Both are mode 0644 and match the unchanged frozen ledger. This is our synthetic
**test app**, not upstream Electron or the official Slack app. It runs through
upstream Electron APIs only during the later protected qualification; recovery
never runs it or copies live app data.

On the builder **host**, with `curl`, Python 3 and `unsquashfs` (the
`squashfs-tools` package) installed, run from a current source copy:

```sh
ASSETS=/srv/tilo/tasks/installer-public-source/target/capture-public-assets
python3 apps/imager/packaging/service-source-assets.py --recover "$ASSETS" \
  > "$ASSETS/source-assets.pending.json" &&
  mv "$ASSETS/source-assets.pending.json" "$ASSETS/source-assets.json"
export TILO_CAPTURE_SOURCE_ASSETS="$ASSETS/source-assets.json"
```

Create `$ASSETS` before shell redirection (`mkdir -p "$ASSETS"`). Recovery
verifies the actual checkout fixture, downloads only the two canonical archive
URLs above if absent, checks their frozen size/mode/hash before extraction, and
measures every extracted file and complete inventory before emitting a manifest.
It copies the verified fixture into `$ASSETS/app`, so all manifest locators stay
on durable host storage. Existing inputs are remeasured, never silently repaired
or repinned. Download failures name the specific URL and leave unadmitted
`.partial` files for inspection; remove those files before retrying a download.
No execution or authentication receipt is produced.

The qualifier's input mount namespace must admit the durable host prefix before
using a `/srv` manifest; recovery alone neither changes that policy nor qualifies
these inputs. Do not relocate them into erased temporary storage to bypass it.

## Audit basis

The frozen ledger was reconstructed from the existing public asset manifest,
not its saved execution claims. Both actual archive hashes and every current
extracted file size/mode/hash were independently remeasured. Electron's complete
ZIP entries were also compared to the ledger; Slack's version was read directly
from the measured `resources/app.asar` package entry. The fixture app hashes were
checked against this repository. Local archive/extraction paths do not appear in
the new subject ledger. This audit supplies no native or provider-authentication
qualification evidence.
