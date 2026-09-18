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

`installed_bytes_verified` and `installable` remain false. The receipt reports
the checks actually performed, retains material-audit coverage/limitations,
and explicitly leaves independent source OCI-layer private-material evidence
and VM boot acceptance outstanding. Unit fixtures do not prove an actual
installed filesystem, loop mount, fsverity, systemd boot, or physical hardware.
