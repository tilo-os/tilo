# Tilo system-extension contract (version 1)

Tilo is an immutable bootc/composefs image. Optional system extensions are
desired state for a future image deployment, never an in-place package-manager
transaction.

`catalog.d/` and `trust.d/` are image-owned inputs: release builds put only
pinned catalogue entries and public verification keys there. They are not
mutable configuration directories, and private signing keys must never enter
the image. `state.json` lives at `/var/lib/tilo/system-extensions/state.json`.
The `system-extension` crate writes it atomically as `root:root` mode `0600`.
It verifies a manifest's Ed25519 signature, an exact base image digest,
optional exact kernel ABI/architecture, and the local artifact receipt before
an extension becomes active desired state.

There are two deliberately different activation classes:

* `local_model_runtime` is userspace-only. The constrained
  `tilo-system-extension-userspace.service` creates a runtime eligibility
  marker. The per-user `tilo-model-manager` independently validates the typed,
  signed local-model catalog and binds vLLM only to an allocated loopback
  endpoint. Its private systemd socket is present at login while the manager
  process remains demand-activated. Settings reaches it through the launcher-owned `models@1` service
  descriptor; it receives no shell command, URL, or ambient app authority.
* `kernel_module` and `gpu_driver_stack` are boot-bound. The constrained
  `tilo-system-extension-boot-stage.service` atomically creates a
  `reboot-required` marker but never touches `/boot` or loads a driver. The
  immutable deployment builder must call `ExtensionState::accepted_for` with
  the exact new deployment digest and kernel ABI before consuming it. The
  resulting deployment is rebooted and covered by ordinary Tilo boot-health and
rollback handling.

## BootPack catalogue consumer

Unprivileged code does not translate PCI IDs to a driver pack. It may ask only
for `boot-pack-recommend`; the root-owned manager reads the local PCI sysfs
facts and verifies a separate image-owned PCI recommendation catalogue signed
by its own pinned public key. Each rule requires an exact vendor/device pair;
an exact subsystem pair wins over a generic device rule, and duplicate rules
are rejected. The response contains only exact BootPack catalogue ids. Settings
or the unprivileged model manager can then enqueue only
`boot-pack-stage <catalog-id>` on the fixed request socket. The catalog id is
an identifier, not a path or image reference. The root-owned
`tilo-system-extension-manager` resolves only
`catalog.d/<catalog-id>.json` and `trust.d/<key-id>.json` from the signed base
image, verifies the manifest signature and exact running image/kernel
compatibility, and accepts only reboot-bound entries whose `artifact.source`
is a `bootc://registry/path@sha256:<digest>` reference matching the signed
artifact digest.

The staging manager repeats the PCI match, so an exact id from another machine
is not authorization to stage a pack here. The socket worker writes only that
id to a root-owned fixed queue and returns `queued`; it has no network. A
separate path-activated manager re-verifies the catalog and then runs the one
fixed operation `bootc switch --transport=registry` with that exact digest
reference. This stages an immutable deployment; it never hot-loads a driver,
runs package-manager commands, or reboots the machine. Tilo never hot-installs a
driver. The non-secret status projection reports `queued`,
`staging`, `staged`, `failed`, or
`active`, plus `activation_class: boot_pack`, the exact extension id, staging
generation, a bounded error if any, and whether a reboot is still required. On
the next boot it is recomputed from the booted deployment digest, so Tilo never
claims a staged driver is already active without evidence.

Settings presents those states as one accelerator-setup card. The model
manager asks for the root-probed recommendation again when the user chooses
setup, and stages only when exactly one signed BootPack matches. Model download
remains disabled while the BootPack is queued or staged. Once staging finishes,
Settings asks the user to restart through the normal system power UI; neither
Settings nor the model service has a restart verb. After boot, local model
setup becomes available only when the root-owned status projection says the
digest-pinned deployment is active.

`/var/lib/tilo/local-models/staging` is root-only transfer workspace.
`verified` is root-owned and group-readable by the local-model runtime; only a
verified backend may promote bytes into it. No helper in this image fetches an
artifact or interprets a manifest as a command.

## Desktop request API

Settings must not run `tilo-model-manager` with privilege or wait for its
synchronous install path. It can read the non-secret status projection at
`/run/tilo/system-extension-api/status.json` and enqueue without waiting by
writing exactly one line to the group-connectable Unix socket
`/run/tilo/system-extension-api/request.sock`, then closing the connection:

* `userspace-stage`
* `boot-stage`
* `boot-pack-recommend`
* `boot-pack-stage <catalog-id>`

The socket is `root:tilo-identity` mode `0660` inside a root-owned non-writable
directory. Its `Accept=yes` systemd service runs in the background and accepts
no other words or fields. In particular there is no `restart` request: a boot
stage only writes `reboot-required`. A future deployment composer may restart
only after it has verified and staged a BootPack from the signed catalogue
against the exact running deployment digest and kernel ABI.

## Downloaded system extensions (`sysext.d/`)

Some optional `/usr` content may not be part of the image: Tilo does not
distribute it, typically because it is patent-encumbered (the media codecs are
the first such extension). The owner turns it on with one action; the device
then downloads the bytes itself, from the publishers' own servers, and
assembles a read-only systemd-sysext from them. The bytes are as trusted as the
image, because the image decides which bytes are acceptable:

1. `sysext.d/<id>.json` is the image-owned catalogue entry. It is part of the
   sealed image, so it carries the image's own signature. It names the
   extension's parts (the systemd-sysext names it may create), the HTTPS hosts
   its artifacts may come from, and where its signed manifest is published.
   It never names a command, a script or a repository.
2. Tilo CI resolves each image's extension against the publishers'
   repositories, checks every package's upstream signature, and publishes a
   manifest that pins every artifact by URL, exact byte length and SHA-256, and
   the exact base it was resolved for. CI signs the manifest bytes with the
   OS release key (the non-exportable KMS key whose public half is
   `keys/cosign.pub`), the same key that authorizes image updates; a manifest
   can only be published where an image signature could be.
3. The device fetches the manifest and its detached signature, verifies the
   signature against `/usr/share/tilo/cosign.pub` (the key this image trusts
   for its own updates) before parsing anything, and then requires: the
   expected `type`, `schema` and `id`; exactly this base (OS, release,
   architecture and the base package fingerprint below); part names and
   artifact hosts allowed by the catalogue entry; and a revision no lower than
   the last one it accepted for this base. Every downloaded artifact must have
   exactly the pinned length and SHA-256. Any mismatch refuses the whole
   transaction and leaves the previous extension in place.

`/usr/libexec/tilo-extension` is the only consumer. It takes exactly one
argument, `<verb>:<id>` (`install`, `remove`, `refresh`, `status`), or `guard`,
or `refresh:all`, and accepts only ids that have a catalogue entry.

### Base package fingerprint

The base an extension was resolved for is identified by the SHA-256 of

    tilo.base-packages.v1\n
    <name> <epochnum>:<version>-<release>.<arch>\n   (one line per package)

over every package in the image's rpm database except `gpg-pubkey`, lines
sorted bytewise. CI computes it inside the published image; the device computes
it from its own (read-only) rpm database. Two images with the same package set
share one manifest.

### Catalogue entry

```json
{
  "schema": 1,
  "id": "media-codecs",
  "kind": "media-codecs",
  "manifest": "https://code.tilo.so/api/packages/tilo/generic/tilo-extension-media-codecs/{arch}-{base}/manifest.json",
  "hosts": ["download1.rpmfusion.org", "codecs.fedoraproject.org"],
  "parts": ["tilo-codecs", "tilo-codecs-mesa"]
}
```

`kind` is display/policy metadata for the desktop; it grants nothing. The
signature is fetched from the manifest URL plus `.sig`. `{arch}` is the rpm
architecture (`x86_64`, `aarch64`), `{base}` the base package fingerprint.

### Signed manifest

The signature is the base64 ECDSA P-256 / SHA-256 signature over the exact
manifest bytes, as `cosign sign-blob` writes it. Verified by
`tilo-crypto verify-p256-sha256 KEY MANIFEST SIGNATURE`.

```json
{
  "type": "tilo.system-extension.sysext-manifest",
  "schema": 1,
  "id": "media-codecs",
  "revision": 20261003120000,
  "created": "2026-10-03T12:00:00Z",
  "base": {
    "os_id": "fedora",
    "version_id": "44",
    "architecture": "x86_64",
    "packages_sha256": "<64 hex>",
    "image": "registry.tilo.so/tilo/tilo@sha256:<64 hex>"
  },
  "parts": [
    {
      "name": "tilo-codecs",
      "format": "rpm",
      "state": "available",
      "message": "",
      "requires_base": {"gstreamer1": "0:1.26.5-1.fc44"},
      "artifacts": [
        {
          "package": "libavcodec-freeworld-7.1.1-2.fc44.x86_64",
          "urls": ["https://download1.rpmfusion.org/free/fedora/updates/44/x86_64/l/libavcodec-freeworld-7.1.1-2.fc44.x86_64.rpm"],
          "size": 4567890,
          "sha256": "<64 hex>",
          "publisher": "RPM Fusion",
          "license": "GPL-3.0-or-later"
        }
      ]
    },
    {
      "name": "tilo-codecs-mesa",
      "format": "rpm",
      "state": "waiting",
      "message": "Hardware H.264/HEVC is waiting for a matching Mesa build from RPM Fusion.",
      "requires_base": {"mesa-dri-drivers": "0:25.2.4-1.fc44"},
      "artifacts": []
    }
  ]
}
```

* `revision` increases with every publication for a base; a manifest is never
  changed once published (`manifest-r<revision>.json` keeps every revision,
  `manifest.json` is the newest).
* `image` is informational; `packages_sha256` is the compatibility pin.
* A part's `state` is `available` (with artifacts), `waiting` (no compatible
  upstream build yet, with a `message`) or `unsupported` (never on this base).
* `format: rpm` is the only assembly format: only `/usr` from each payload is
  used, RPM scriptlets never run, set-id bits are dropped, and a private
  library directory a package registers in `/etc/ld.so.conf.d` is moved to the
  base's own library path (an extension cannot change `/etc`).
* `requires_base` lists exact base package versions (`epochnum:version-release`)
  the part was built against. The boot guard, offline, sets aside any part
  whose requirements the booted base does not meet, or that contains a package
  the booted base now has, and restores a set-aside part that matches again
  (after a rollback). Each part's extension-release carries `ID`,
  `VERSION_ID` and `ARCHITECTURE` of the base, so systemd-sysext itself refuses
  a part built for another OS release.

### Device state

| Path | What |
| --- | --- |
| `/var/lib/tilo/extensions/<id>.json` | Status, world-readable, replaced atomically |
| `/var/lib/tilo/extensions/<id>/enabled` | The owner's opt-in |
| `/var/lib/tilo/extensions/<id>/accepted.json` | Highest accepted revision per base |
| `/var/lib/tilo/extensions/<id>/parked/` | Parts set aside by the boot guard |
| `/var/cache/tilo-extension/objects/<sha256>` | Verified artifacts, content-addressed |
| `/var/cache/tilo-extension/partial/<sha256>` | Downloads in progress, resumed by byte offset |
| `/var/lib/extensions/<part>` | The assembled, read-only parts systemd-sysext merges |
| `/etc/tilo/extensions/opt-in/<id>` | The installer's handoff of an opt-in, adopted once |

Status JSON: `schema`, `id`, `state` (`not-installed`, `waiting-for-network`,
`installing`, `installed`, `removing`, `failed`), `desired`, `progress`
(`{"step", "steps", "label"}` while installing), `parts` (part name to
`enabled`, `waiting`, `unsupported` or `off`), `message`, `last_error`,
`revision`, `base`, `packages` (part name to package list), `generation`
(increases after every merge or unmerge) and `updated`.

Units: `tilo-extension@<verb>:<id>.service` (polkit lets a local, active
administrator start only `install`, `remove` and `refresh` instances),
`tilo-extension-guard.service` (every boot, offline, before
`systemd-sysext.service`), `tilo-extension-refresh.timer` (after boot, daily,
and on every update check), and NetworkManager's `90-tilo-extension`
(an opt-in made offline installs once the device is online).
