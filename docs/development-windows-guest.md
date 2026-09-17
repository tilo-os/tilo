# Assemble a development Windows guest

The ARM image carries a separate x86 Windows compatibility rootfs. Its current
consumer requires the traditional Wine pair, including both x86-64 and i686
loaders. Fedora's stock Wine packages do not satisfy that consumer contract.

Run **Development / assemble retained Windows guest** on a `codex/` branch after
the Wine producer has published its paired artifact. This workflow packages
existing Wine RPMs; it does not compile Wine, the browser or the OS.

## Inputs and reuse

- `buildroot_candidate`: immutable native x86-64 Fedora 44 tooling image.
- First assembly: supply `wine_pair_candidate` from the Wine producer's pair job.
- Later assembly: supply `signed_wine_candidate` and its independent
  `signed_wine_key_sha256` from the retained public receipt instead of the pair.
- To reuse dependency resolution too, supply `guest_rpms_candidate` with the same
  signed Wine identity and public-key hash used to create that snapshot.

Each completed stage publishes its digest immediately. The always-retained
`guest-evidence/inputs.json` records completed inputs, even if a later stage
fails. Use those references for the next attempt. Only the pinned DXVK archive
and license are fetched for this guest.

The final `guest-reference.txt` is the immutable `windows_guest_candidate` input
for the ARM development OS build. Assembly validates the installed rootfs; it
does not establish Windows application or FEX runtime acceptance.

## Development trust

The initial packaging step generates a temporary development RPM signing key
inside a disposable container, signs copies of the validated Wine RPMs and
exports only signed RPMs, the public key and provenance. The private key is
removed. This makes the artifact reproducibly identifiable by its published
digest, not byte-reproducible across fresh signing runs.

The public-key hash is an explicit input to subsequent stages. Custom Wine and
Fedora packages are verified against separate trust databases before assembly;
Fedora package signature checks remain enabled. These development keys do not
provide a production release identity or Secure Boot trust.

Production needs a persistent protected signing identity, an authorized release
job, independently distributed public trust anchors, and rotation/revocation
procedures. RPM signing, container-image signing, and firmware/boot signing are
separate trust paths; configuring one does not configure the others. Development
guest packaging does not enroll keys or change the PC's firmware.
