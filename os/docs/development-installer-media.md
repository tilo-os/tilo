# Development installation media without recompiling applications

Use the finalization request with explicit `boot_profile: installer-media`.
Its input is the same authenticated retained **raw rootfs receipt** accepted by
normal finalization. Do not pass a previously finalized installer image as raw.
The existing importer still rejects raw inputs containing completed boot files.

The driver authenticates/rebinds the raw rootfs and signs its loader normally.
Within provisional stage3 it first creates the original provisional UKIs, then
layers the installer plumbing on that exact immutable local image. Stage4
computes composefs only after every `/usr` overlay change. Reseal and signing
then cover both live and installed-target UKIs. Final verification requires one
authenticated live UKI with exactly one bare `tilo.install` and an independently
predicted target UKI without that flag. Source seal, generation and signatures
are checked using the existing boot-selection verifier.

The overlay takes `os/usr` and both greetd configurations from the actual
request source, preserving its runtime/security policy. Only the reviewed
`tilo-install`, `tilo-verified-boot-install-preflight`, UKI command-line tool and
installer Containerfile come from the exact coherent helper revision. Stage3
records every overlay input SHA256 and both revision identities. It inherits
`/usr/bin/tilo-installer` from the admitted native payload; a missing executable
fails the installer Containerfile. No Cargo build or substitute UI is used.

The medium is published at its own immutable digest and request-specific
staging tag. `new_target` remains the installed OS update origin, not a tag to
move to installer media. Never promote the live-media digest to that normal OS
update tag. Separate desktop/media requests have distinct hashes and evidence
directories; receipt replay across modes fails the existing request binding.

After independently signing/admitting the medium's published OCI identity,
create it using `create-development-disk.py --boot-profile installer-media`
with the exact authenticated live UKI path from final verification. This writes
only an owned image file and materializes target EFI/prediction artifacts.
The installed-content verifier must match the medium profile. Disk creation,
source policy tests, VM boot and actual GUI install remain unperformed until
real signed artifacts exist.

`build-custom-installer.sh --reuse-image-installer --image-only` remains a
standalone intermediate-image developer tool. Its output has an invalidated
seal and is not an input to the raw-rootfs finalizer. The integrated media mode
above is the executable signed-development route.
