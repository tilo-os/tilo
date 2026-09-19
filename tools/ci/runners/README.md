# Ephemeral CI runner launchers

These launchers admit exactly one native imager job on a disposable host and
remove its registration afterward. They are host-provisioning inputs, not
scripts to execute from an untrusted workflow checkout.

- `imager-ephemeral-linux.sh` is installed as a root-owned, non-writable Linux
  executable.
- `imager-ephemeral-windows.ps1` and `forgejo-runner.exe` are installed as
  administrator-owned files on the disposable Windows image.

The launchers validate their runner binaries, registration metadata, labels,
ownership, and cleanup boundaries before admitting work. Keep the Linux and
Windows security-contract tests in `os/tests/` aligned with changes here.
