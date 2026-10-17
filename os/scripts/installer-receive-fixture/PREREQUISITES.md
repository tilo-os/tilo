The native receiving fixture runs in a scoped prerequisite image, derived from
the immutable Fedora 44 builder image. It is not an OS build. The measured
candidate overlay supplies the current Shell, filesystem, control CLI,
credential/prepared helpers, Files, Writer, packaging CLIs and signed SDK.
No candidate executable is taken from the prerequisite image.

Create an exclusive context with:

```sh
python3 os/scripts/installer-receive-fixture/prerequisite-image.py \
  --output "${TMPDIR:?}/tilo-native-prerequisite-context"
```

If the historical local base is unavailable, use `--base` with an independently
verified immutable Fedora image ID or registry digest reference (never a mutable
tag). The generated recipe and source manifest record that exact base. Resolve
and verify the registry manifest before pulling, retain the recipe, and tag the
resulting prerequisite image so ordinary dangling-image cleanup cannot remove
it during qualification.

The generated Containerfile installs normal Fedora Flatpak, OSTree, D-Bus,
Polkit, UDisks, udev, Secret Service, PAM and storage dependencies. It retains
the distribution's login/PAM/authselect configuration. It copies the actual
firstboot, Home-seed, prepared-extractor, handoff and store-volume helpers;
Tilo unlock PAM, sysusers and tmpfiles configurations; and the committed
registry runtime publisher root used by the normal OS image. It invokes the real sysusers
configuration to create `tilo-identity`. It creates no login account,
password, credential, fake authentication policy or migration receipt.

The real fixed-hash Flatpak fixture app and reviewed runtime preparer are
included. The preparer and its three closed dependencies were retained from
`manifest-0dece0b6.json` in the prior runnable Flatpak qualification work and
ported without changing their bytes. The preparer only collects the actual
image Bash/sleep ELF dependency closure into the private source namespace;
it does not download a runtime or execute an app.

Build the generated context in the operator's rootless Podman store. This is a
text-only qualification prerequisite container, not a Tilo OS image build.
The build emits `/usr/share/tilo-native-prerequisites/source-inputs.json` with
each installed text source hash and `/usr/share/tilo-native-prerequisites/observed-readiness.json`
with actual tools, maintained PAM hashes and installed RPM versions. Run
`/usr/libexec/tilo-native-prerequisite-readiness` in a fresh container to
reobserve those subjects. These observations explicitly report
`qualification: false`. Candidate qualification still needs the actual
authenticated source capture, firstboot/import and ordinary receiving app
actions; prerequisite presence alone proves none of those.

The 2026-10-11 scoped build passed at immutable image
`sha256:ea5ddbc449cb8e81b1cd79307922307b04907f00467c749551f0094811659225`,
derived from
`sha256:ce2ba1165f919362e3e617456def5e6c20d936aa7ba418074602c2bc40a92e91`.
A fresh network-disabled container matched the build's 32 tool observations,
PAM hashes, source hashes and 474-package inventory. The reviewed runtime
preparer then observed five actual ELF entries totaling 5,259,608 bytes;
its report confirms `native_app_executed: false`. Full migration qualification
has not run. A changed staged source requires a new context and image;
do not relabel this image with a newer source cohort.
