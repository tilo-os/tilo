# Installed development disk verification

`verify-installed-development-disk.py` runs after successful creation, inside
the same dedicated native ARM guest. It requires the creator's new
`file_identity` receipt field and an unchanged private output directory.
Receipts are evidence, not authorization: source signature and boot prediction
verification run again against independently supplied inputs.

Run the trusted helper under `unshare --mount --propagation private --` with
the normal source verification arguments, `--generation`, `--composefs-digest`,
`--systemd-version`, `--disk /private/output/disk.raw`, and
`--creation-receipt /private/output/creation.json`. Registry credentials stay
outside the installed image. The process requires preinstalled trusted native
bootc 1.16.4 (including `internals cfs`), Podman, systemd-measure, signature tools,
util-linux and filesystem support. It never installs tools or compiles code.

The verifier opens the exact recorded regular file read-only through a held
descriptor, allocates only its own read-only loop device, validates the two
expected ARM GPT partitions, mounts them read-only, and tears down every mount
and the loop before returning success. It reauthenticates the selected UKI by
byte identity to the freshly signature/PCR-verified source, checks both signed
loader copies, origin and boot digest, performs installed OCI fsck, and mounts
the exact composefs seal with kernel mounting, required verity and no repository
upgrade. Immutable profile/generation and effective mutable signature policy
must match. The private-material module checks actual mutable etc, shared var,
ESP and selected initramfs under its explicitly bounded policy.

This first implementation deliberately supports the current direct ARM
Btrfs+systemd+composefs layout only. Different actual layout/entry defaults fail
closed and must be reconciled with real bootc output before acceptance; do not
weaken checks merely to obtain a successful receipt.

The default `--boot-profile installed-target` requires the selected installed
UKI to match the independently PCR-verified `/boot/tilo-target/tilo-target.efi`
and rejects installer activation. For a medium use
`--boot-profile installer-media --source-live-uki boot/EFI/Linux/EXACT-LIVE.efi`.
The named source live UKI must appear exactly once in fresh authenticated EFI
evidence, match its recorded bytes, bind the same composefs seal/generation,
and contain exactly one bare `tilo.install` token. The actual selected ESP UKI
must match those live bytes. Arbitrary EFI paths, omitted explicit selection,
valued/duplicate installer flags and a target UKI substituted for live are
rejected. The separate normal target must still omit installer activation and
pass full target PCR prediction verification.

The receipt's `selected_boot` describes the actual disk UKI and whether the
target PCR proof applies to it. For media this field is false: target PCR
evidence is retained separately under `boot`, never attributed to the live UKI.
`sign-target-prediction.sh` signs every live UKI and the separate normal target,
but produces PCR sidecars only for the target. Both medium live boot and final
target boot remain independent runtime acceptance requirements.

`installed_bytes_verified` and `installable` remain false. The receipt reports
the checks actually performed, retains material-audit coverage/limitations,
and explicitly leaves independent source OCI-layer private-material evidence
and VM boot acceptance outstanding. Unit fixtures do not prove an actual
installed filesystem, loop mount, fsverity, systemd boot, or physical hardware.

Loader selection follows the actual bootc1.16.4 call order: `install_systemd_boot`
runs before `setup_composefs_uki_boot`; the latter preserves existing
`loader/loader.conf`. systemd v259 `install_loader_config` writes a commented
timeout/console template and optionally `default ENTRY_TOKEN-*`. With exactly
one suitable installed entry and no alternate EFI applications, a matching
token selects it and a nonmatching token falls back to it (`config_select_default_entry`).
Firmware variable overrides remain outside static disk proof and require VM
boot verification. Sources: bootc `bootc_composefs/boot.rs`, systemd v259
`src/bootctl/bootctl-install.c` and `src/boot/boot.c`.

Loader verification uses the authenticated source's `.efi.signed` companion,
the original unsigned loader, and the independently pinned certificate with
the existing Authenticode-envelope verifier. Both installed copies must match
the resulting signed-byte digest. Local disk-tool commands have a 120-second
deadline (one hour for installed repository fsck), a 4-MiB stdout bound, and
process-group cleanup. Errors do not expose tool stderr or credential contents.
