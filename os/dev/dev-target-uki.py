#!/usr/bin/env python3
"""Build a split-kernel development target's UKI on the CI runner.

Generic tilo-vm dev images carry a split kernel and no UKI, and the live
installer medium deliberately ships no UKI constructor. So the runner that
assembles a dev installer medium builds the target's UKI once, from the exact
kernel, initramfs and pinned hardware DTB of the selected image, and carries it
beside the target's OCI layout. The VM installer only verifies that payload
against the installed image and writes the installed command line into the room
reserved here (`tilo-installer-offline-target dev-target-uki`,
crates/system/install-helpers/src/dev_target_uki.rs).

The OCI image is not changed: its manifest digest is the target's identity. The
constructor is the boot adapter's `uki-tools` stage
(os/Containerfile.dev-installer-base) layered on the target, which installs the
frozen RPM snapshot's ukify and EFI stub. The pristine target is mounted
read-only beside it, so the UKI never reads the tool layer's files.

    dev-target-uki.py construct ROOT OUTPUT   (inside the tool container)
"""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

OUTPUT_NAME = "target-uki.efi"
# Must equal CMDLINE_ROOM in crates/system/install-helpers/src/dev_target_uki.rs,
# whose tests read it from here. One token of 2047 bytes: arm64's
# COMMAND_LINE_SIZE bounds what the installer writes back.
CMDLINE_ROOM = "tilo.dev-target-cmdline-room"
CMDLINE_BYTES = 2047
ADAPTER = "os/Containerfile.dev-installer-base"
RPMS_REF = re.compile(r"registry\.tilo\.so/tilo/rpms@sha256:[0-9a-f]{64}\Z")
LAYOUT_PROBE = r"""
test "$(cat /usr/lib/tilo/image-flavour)" = dev
if [ -e /boot/tilo-target ]; then echo bundle; exit 0; fi
set -- /boot/EFI/Linux/*.efi
if [ "$#" = 1 ] && [ "$1" = "/boot/EFI/Linux/*.efi" ] && [ ! -e "$1" ]; then
  echo unsealed
else
  echo sealed
fi
"""


def placeholder():
    token = CMDLINE_ROOM + "="
    return token + "x" * (CMDLINE_BYTES - len(token))


def run(argv, **kwargs):
    return subprocess.run([str(item) for item in argv], check=True, **kwargs)


def layout(image):
    """bundle, sealed or unsealed: only an unsealed split kernel needs a UKI."""
    result = run(["podman", "run", "--rm", "--pull=never", "--network=none",
                  "--entrypoint", "/bin/sh", image, "-ec", LAYOUT_PROBE],
                 capture_output=True, text=True)
    value = result.stdout.strip()
    if value not in ("bundle", "sealed", "unsealed"):
        raise ValueError(f"development target boot layout is ambiguous: {value!r}")
    return value


def tool_stage_sources(containerfile):
    """The repository files the adapter's uki-tools stage COPYs, read from it."""
    sources, inside = [], False
    for line in containerfile.read_text().splitlines():
        words = line.split()
        if words[:1] == ["FROM"]:
            inside = words[-2:] == ["AS", "uki-tools"]
        elif inside and words[:1] == ["COPY"]:
            sources += [word for word in words[1:-1] if not word.startswith("--")]
    if not sources or any(not re.fullmatch(r"os/dev/[A-Za-z0-9_.-]+", item) for item in sources):
        raise ValueError(f"{ADAPTER} has no recognizable uki-tools stage")
    return sources


def build(image, rpms_ref, output, repo):
    """Write OUTPUT for an unsealed target; return False when it needs none."""
    run(["podman", "pull", image])
    if layout(image) != "unsealed":
        return False
    if rpms_ref is None or RPMS_REF.fullmatch(rpms_ref) is None:
        raise ValueError("a split-kernel development target needs the digest-pinned RPM snapshot")
    if output.exists() or output.is_symlink():
        raise ValueError("development target UKI output already exists")
    tools = "localhost/tilo-dev-target-uki-tools:" + image.rsplit(":", 1)[1][:16]
    with tempfile.TemporaryDirectory(prefix="tilo-dev-target-uki-") as temporary:
        context = Path(temporary) / "context"
        (context / "os/dev").mkdir(parents=True)
        for source in tool_stage_sources(repo / ADAPTER):
            shutil.copy2(repo / source, context / source)
        run(["podman", "build", "--target", "uki-tools", "--build-arg", "BASE_IMAGE=" + image,
             "--build-arg", "RPMS_REF=" + rpms_ref, "-t", tools,
             "-f", repo / ADAPTER, context])
        scratch = Path(temporary) / "out"
        scratch.mkdir()
        try:
            run(["podman", "run", "--rm", "--pull=never", "--network=none",
                 "--security-opt", "label=disable",
                 "--mount", f"type=image,source={image},destination=/target",
                 "-v", f"{Path(__file__).resolve()}:/usr/local/libexec/dev-target-uki.py:ro",
                 "-v", f"{scratch}:/out", "--entrypoint", "python3", tools,
                 "/usr/local/libexec/dev-target-uki.py", "construct", "/target", "/out/" + OUTPUT_NAME])
        finally:
            subprocess.run(["podman", "rmi", "-f", tools], check=False)
        built = scratch / OUTPUT_NAME
        if not built.is_file() or built.is_symlink() or built.stat().st_size == 0:
            raise ValueError("UKI constructor produced no target UKI")
        shutil.copyfile(built, output)
    output.chmod(0o644)
    return True


def kernel_inputs(root):
    modules = root / "usr/lib/modules"
    versions = [path for path in modules.iterdir() if path.is_dir() and not path.is_symlink()]
    if len(versions) != 1 or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._+-]*", versions[0].name):
        raise ValueError("development target requires exactly one kernel")
    version = versions[0].name
    directory = modules / version
    kernel, initrd = directory / "vmlinuz", directory / "initramfs.img"
    for path in (kernel, initrd):
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"missing target boot input: {path.relative_to(root)}")
    return version, kernel, initrd


def construct(root, output):
    """Inside the uki-tools container: ukify over the pristine mounted target."""
    if (root / "usr/lib/tilo/image-flavour").read_bytes() != b"dev\n":
        raise ValueError("target is not development flavour")
    version, kernel, initrd = kernel_inputs(root)
    dtb = run(["python3", "/usr/local/bin/uki-hardware-profile.py", root, version, "aarch64"],
              capture_output=True, text=True).stdout.strip()
    with tempfile.TemporaryDirectory() as temporary:
        cmdline = Path(temporary) / "cmdline"
        cmdline.write_text(placeholder() + "\n")
        arguments = ["ukify", "build", "--linux", kernel, "--initrd", initrd, "--uname", version,
                     "--os-release", "@" + str(root / "usr/lib/os-release"),
                     "--cmdline", "@" + str(cmdline), "--output", output]
        if dtb:
            if not Path(dtb).resolve().is_relative_to(root.resolve()):
                raise ValueError("hardware profile DTB is outside the target")
            arguments += ["--devicetree", dtb]
        run(arguments)


def main(argv):
    if len(argv) != 4 or argv[1] != "construct":
        raise SystemExit(__doc__.rstrip().splitlines()[-1].strip())
    try:
        construct(Path(argv[2]), Path(argv[3]))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"development target UKI: {error}") from error
    os.chmod(argv[3], 0o644)


if __name__ == "__main__":
    main(sys.argv)
