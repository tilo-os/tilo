#!/usr/bin/env python3
"""Fail-closed OS Windows provider inputs; never execute guest payloads."""

import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys


def load(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def reference(value):
    if not re.fullmatch(r"[^\s@]+@sha256:[0-9a-f]{64}", value):
        raise ValueError("ARM Windows payloads require immutable OCI sha256 references")
    return value


def mode(arch, fex_ref, guest_ref):
    """Whether this build includes the Windows provider: "1" or "0".

    x86_64 always ships Fedora Wine. ARM ships FEX and the x86 guest exactly
    when both are pinned, and builds without Windows apps while neither is.
    A single pin is an incomplete provider, never a reason to drop it.
    """
    if arch == "x86_64":
        return "1"
    if arch != "aarch64":
        raise ValueError("unsupported Windows provider architecture")
    pinned = [value not in ("", "scratch") for value in (fex_ref, guest_ref)]
    if all(pinned):
        inputs(arch, fex_ref, guest_ref)
        return "1"
    if any(pinned):
        raise ValueError("ARM Windows provider needs both FEX_PAYLOAD_REF and WINDOWS_GUEST_REF")
    return "0"


def absent(arch):
    """Build record of an ARM image assembled without the Windows provider."""
    if arch != "aarch64":
        raise ValueError(f"{arch} images always include the Windows provider")
    return {"schema_version": 1, "host_architecture": arch, "provider": "none",
            "detail": "Built without FEX_PAYLOAD_REF and WINDOWS_GUEST_REF; "
                      "Windows apps are not available on this image."}


def inputs(arch, fex_ref, guest_ref):
    if arch not in ("x86_64", "aarch64"):
        raise ValueError("unsupported Windows provider architecture")
    result = {"schema_version": 1, "host_architecture": arch}
    if arch == "aarch64":
        result.update(provider="fex-traditional-wine", fex_ref=reference(fex_ref),
                      guest_ref=reference(guest_ref))
    else:
        result["provider"] = "native-wine"
    return result


def traditional_wine(root, guest, pin):
    """Check the Fedora spec's supported cross-width Unix loader layout."""
    records = []
    for name, canonical, bits, machine in (
        ("usr/lib64/wine/i386-unix/wine", "usr/lib/wine/i386-unix/wine", 32, 3),
        ("usr/lib/wine/x86_64-unix/wine", "usr/lib64/wine/x86_64-unix/wine", 64, 62),
    ):
        path = guest.guest_file(root, name)
        # Fedora's cross-width directory aliases must reach the corresponding
        # Unix loader. Wine 11 builds /usr/bin/wine{32,64} from tools/wine,
        # separately from loader/main.c; guest.validate checks those wrappers.
        if path != guest.guest_file(root, canonical):
            raise ValueError("alternate Wine loader does not resolve to the canonical Unix loader")
        records.append({"path": name, "canonical_path": canonical,
                        "resolved_path": path.relative_to(root).as_posix(),
                        "sha256": file_hash(path),
                        **guest.elf_info(path, bits, machine, True)})
    packages = guest.guest_file(root, "usr/lib/tilo/windows-guest/installed-packages.tsv")
    cores = []
    for line in packages.read_text().splitlines():
        fields = line.split("\t")
        if len(fields) != 6:
            raise ValueError("malformed guest RPM inventory")
        if fields[0] == "wine-core":
            cores.append((fields[2], fields[3], fields[4]))
    if sorted(cores) != sorted((pin["version"], pin["build_release"], arch)
                               for arch in ("i686", "x86_64")):
        raise ValueError("guest must contain the pinned Tilo traditional Wine pair")
    return records


def validate(fex_root, guest_root, fex_ref, guest_ref, source_pin, wine_pin):
    result = inputs("aarch64", fex_ref, guest_ref)
    fex, guest = load("validate-fex-payload"), load("validate-windows-guest")
    fex_root, guest_root = Path(fex_root).resolve(), Path(guest_root).resolve()
    result["fex_payload"] = fex.validate(fex_root)
    result["guest_payload"] = guest.validate(guest_root)
    # Producer inventories must match the consumer's independent validation.
    for root, name, expected in (
        (fex_root, "usr/lib/tilo/fex/payload-inventory.json", result["fex_payload"]),
        (guest_root, "usr/lib/tilo/windows-guest/payload.json", result["guest_payload"]),
    ):
        if json.loads(guest.guest_file(root, name).read_text()) != expected:
            raise ValueError(f"stale or mismatched producer inventory: {name}")
    revision = re.search(r"^FEX_SOURCE_REVISION=([0-9a-f]{40})$", source_pin.read_text(), re.M)
    source = guest.guest_file(fex_root, "usr/lib/tilo/fex/source.txt").read_text()
    if not revision or source.splitlines()[0] != "FEX source revision: " + revision[1]:
        raise ValueError("FEX source differs from the repository pin")
    patch_module = load("apply-fex-patches")
    patches = patch_module.patch_record(source_pin.parent / "fex-patches")
    recorded_patches = json.loads(guest.guest_file(fex_root,
        "usr/lib/tilo/fex/downstream-patches.json").read_text())
    shipped_patches = patch_module.patch_record(guest.guest_file(fex_root,
        "usr/share/tilo/fex/patches/series.json").parent)
    if patches["base_revision"] != revision[1] or recorded_patches != patches or shipped_patches != patches:
        raise ValueError("FEX downstream patches differ from the reviewed series")
    result["fex_downstream_patches"] = patches
    cache = guest.guest_file(fex_root, "usr/lib/tilo/fex/cmake-cache.txt").read_text()
    for key, value in (("TUNE_CPU", "none"), ("TUNE_ARCH", "generic")):
        if not re.search(rf"^{key}:[^=]+={value}$", cache, re.M):
            raise ValueError("FEX payload is not a portable CPU build")
    for name in ("usr/lib/tilo/fex/inputs.env", "usr/lib/tilo/fex/buildroot-packages.txt"):
        guest.guest_file(fex_root, name)
    upstream_inputs = guest.guest_file(fex_root, "usr/lib/tilo/fex/inputs.env").read_text()
    for key in ("FEX_BUILDROOT_REF", "FEX_X86_SDK_REF"):
        matches = re.findall(rf"^{key}=(.+)$", upstream_inputs, re.M)
        if len(matches) != 1:
            raise ValueError("missing or duplicate FEX build provenance")
        reference(matches[0])
    for name in ("windows-guest-contract.json", "windows-guest-roots.txt", "rpms-ref.txt"):
        guest.guest_file(guest_root, "usr/lib/tilo/windows-guest/" + name)
    snapshot = json.loads(guest.guest_file(guest_root,
        "usr/lib/tilo/windows-guest/windows-guest-contract.json").read_text())
    if (snapshot.get("transaction_base"), snapshot.get("architecture"), snapshot.get("releasever"),
            snapshot.get("install_weak_deps")) != ("empty-installroot", "x86_64", "44", True):
        raise ValueError("guest is not a complete Fedora 44 transaction snapshot")
    reference(snapshot.get("buildroot_ref", ""))
    reference(guest.guest_file(guest_root, "usr/lib/tilo/windows-guest/rpms-ref.txt").read_text().strip())
    license_path = guest.guest_file(guest_root, "usr/share/licenses/tilo-dxvk/LICENSE")
    dxvk = json.loads(guest.guest_file(guest_root, "usr/share/tilo/dxvk/provenance.json").read_text())
    if dxvk.get("license", {}).get("sha256") != file_hash(license_path):
        raise ValueError("DXVK license differs from recorded provenance")
    result["native_elfs"] = []
    for directory in ("usr/bin", "usr/lib64"):
        for path in sorted((fex_root / directory).rglob("*")):
            if not path.is_file():
                continue
            name = path.relative_to(fex_root).as_posix()
            path = fex.regular_file(fex_root, name)
            with path.open("rb") as stream:
                if stream.read(4) != b"\x7fELF":
                    continue
            if fex.elf_header(path, name)[:2] != (2, 183):
                raise ValueError(f"non-native ELF in FEX host payload: {name}")
            result["native_elfs"].append({"path": name, "sha256": file_hash(path)})
    result["traditional_loaders"] = traditional_wine(guest_root, guest, json.loads(wine_pin.read_text()))
    result["limitations"] = ["Packaging checks do not replace Wine32, Wine64, mixed-prefix, Proton or GPU execution acceptance."]
    return result


def file_hash(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def validate_host(manifest, root=Path("/")):
    """Check native files and their dynamic linker closure under `root`.

    `root` is `/` for a booted host. The image build checks the Windows
    compatibility payload before it is packed as a system extension, so there
    the payload tree is `root` and its own library directories are searched in
    addition to the base's.
    """
    data = json.loads(manifest.read_text())
    if data["host_architecture"] == "x86_64" or data["provider"] == "none":
        return data
    for item in data["native_elfs"]:
        path = root / item["path"]
        if file_hash(path) != item["sha256"]:
            raise ValueError(f"copied FEX ELF differs from validated artifact: {path}")
        command = ["/lib/ld-linux-aarch64.so.1"]
        if root != Path("/"):
            library_path = ":".join(str(root / "usr" / name) for name in ("lib64", "lib"))
            command += ["--library-path", library_path]
        subprocess.run([*command, "--list", str(path)], check=True, stdout=subprocess.DEVNULL)
    return {"validation": "native-dynamic-linker-closure", "files": len(data["native_elfs"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("mode", "absent", "inputs", "validate", "host"))
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--arch", default="aarch64")
    parser.add_argument("--fex-ref", default="")
    parser.add_argument("--guest-ref", default="")
    parser.add_argument("--fex-root", type=Path)
    parser.add_argument("--guest-root", type=Path)
    parser.add_argument("--source-pin", type=Path)
    parser.add_argument("--wine-pin", type=Path)
    parser.add_argument("--root", type=Path, default=Path("/"))
    args = parser.parse_args()
    try:
        if args.action == "host":
            if args.manifest is None:
                raise ValueError("host validation requires a manifest")
            print(json.dumps(validate_host(args.manifest, args.root)))
            return 0
        if args.action == "mode":
            print(mode(args.arch, args.fex_ref, args.guest_ref))
            return 0
        if args.action == "absent":
            print(json.dumps(absent(args.arch), indent=2, sort_keys=True))
            return 0
        result = inputs(args.arch, args.fex_ref, args.guest_ref)
        if args.action == "validate":
            if args.arch != "aarch64" or any(value is None for value in
                    (args.fex_root, args.guest_root, args.source_pin, args.wine_pin)):
                raise ValueError("ARM validation requires both payload roots and source pins")
            result = validate(args.fex_root, args.guest_root, args.fex_ref, args.guest_ref,
                              args.source_pin, args.wine_pin)
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (ValueError, OSError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Windows provider rejected: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
