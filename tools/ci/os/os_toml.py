#!/usr/bin/env python3
"""The OS image's checks, written into t.toml from os/image/layers.toml.

    os_toml.py            print the block (tools/ci/t_toml.py writes it as `os-layers`)

Per architecture: the external assets, the assembler (the image the actions run in),
the base, the OS builder and side builds, the compile (in the OS builder, with t's
compiler cache), one check per layer, and the composition, which is `os-image-<arch>`;
then `os-image`, the index of both. The `dev` variant is the development loop and main's
integration image; the `release` variant is what stable promotes, built on main every
night as capacity allows and found in the cache by stable's run of the same commit.

Each layer check is keyed by exactly what its piece reads: its Containerfile and the
helper stages it builds on, every file those COPY or bind-mount, the tools that build
and verify a layer, and the layers below it (its needs). So a changed unit file reruns
the system layer and what is above it, a changed crate the compile and the layers from
the binaries up, and neither ever the package transaction.
"""

import os
import re
import sys
import tomllib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pieces  # noqa: E402

REPO = pieces.REPO
ARCHES = [("x86_64", "arch=x86_64"), ("aarch64", "arch=arm64")]
VARIANTS = ["dev", "release"]
SHARED = {"packages", "kernel", "system"}
ASSEMBLER_IMAGE = '{{ check = "os-assembler-{arch}", path = "/target/os/{arch}/assembler/oci" }}'
BUILDER_IMAGE = '{{ check = "os-builder-{arch}", path = "/target/os/{arch}/builder/builder-base/oci" }}'
SKOPEO = "registry.tilo.so/tilo/skopeo@sha256:7e735527d9d64c7456fc82d7f954c380b4a5dd1a8173115ce2ed7b3442ef36e0"
# The tools every root action builds and verifies through.
TOOLS = ["os/image/build-layer.sh", "os/image/layers.toml", "os/image/args.Containerfile",
         "tools/ci/os/pieces.py", "tools/ci/os/oci_layers.py", "tools/ci/publish/lib.sh",
         "os/base-snapshot.env", "os/kernel-snapshot.env", "os/dev/build-image.sh", "keys/cosign.pub"]
# Release nightly on main as capacity allows; stable's run finds it in the cache.
RELEASE_ON = '["schedule:0 2 * * *", "stable", "dispatch"]'


def scripts_of(paths, depth=3):
    """The os/ scripts the given scripts run, transitively: build-image.sh resolves and
    verifies every pin through them, so they key every layer."""
    found, todo = set(), list(paths)
    pat = re.compile(r"(?:\$TILO_OS_DIR|\$\{TILO_OS_DIR\}|os)/((?:dev|tests|scripts)/[A-Za-z0-9_.-]+\.(?:sh|py))")
    for _ in range(depth):
        nxt = []
        for p in todo:
            full = os.path.join(REPO, p)
            if not os.path.isfile(full):
                continue
            for m in pat.finditer(open(full, errors="replace").read()):
                q = "os/" + m.group(1)
                if q not in found and os.path.isfile(os.path.join(REPO, q)):
                    found.add(q)
                    nxt.append(q)
        todo = nxt
    return sorted(found)


def arr(items):
    return "[" + ", ".join(f'"{i}"' for i in items) + "]"


def check(fields):
    out = ["[[check]]"]
    for k, v in fields:
        out.append(f"{k} = {v}")
    return "\n".join(out) + "\n"


def piece_files(name, given):
    """The piece's own file and the helper files it builds on."""
    plan = pieces.plan()
    text = pieces.action_text(name)
    files = []
    for l in plan["layer"]:
        if l["name"] == name:
            files.append(f"os/image/{l['piece']}.Containerfile")
    for h in pieces.needed_helpers(text, given):
        files.append(f"os/image/helpers/{h}.Containerfile")
    return files


def given_stages(layer):
    stages = []
    for g in layer.get("given", []):
        if g != "compiled":
            stages.append(g.split(":", 1)[1])
    return stages


def layer_check_name(name, arch, variant):
    if name in SHARED or variant == "dev":
        return f"os-layer-{name}-{arch}"
    return f"os-layer-{name}-{variant}-{arch}"


def layer_out(name, arch, variant):
    if name in SHARED:
        return f"/target/os/{arch}/layers/{name}"
    return f"/target/os/{arch}/{variant}/layers/{name}"


def block():
    plan = pieces.plan()
    tools = TOOLS + scripts_of(TOOLS)
    out = ["""# The OS image as layer actions (os/image/layers.toml, docs: os/image/args.Containerfile and
# t's docs/images.md): per architecture the assembler the actions run in, the pinned base, the
# OS builder and the side builds, the compile in the OS builder with t's compiler cache, one
# check per layer, each keyed by exactly what its piece reads, and the composition, os-image-<arch>,
# which writes a manifest over the layers' blobs in seconds. A changed crate reruns the compile,
# the binaries' layers and the derived and boot layers above them, never the packages, kernel or
# system layers. `-release-` checks are the variant stable promotes.
""",
           check([("name", '"os-assets"'),
                  ("run", '"bash os/image/build-layer.sh assets"'),
                  ("inputs", arr(["os/external-assets/github-assets.tsv", "os/dev/cache-github-assets.sh", "os/image/build-layer.sh"])),
                  ("outputs", arr(["os/external-assets/cache/**"])),
                  ("cost", '"minutes"'), ("default", "false")])]
    builder_sources = pieces.inputs_of_text(pieces.helpers()["builder-sources"])
    for arch, node in ARCHES:
        req = arr([node, "disk-100g"])
        out.append(check([
            ("name", f'"os-assembler-{arch}"'),
            ("run", f'"bash os/image/assembler.sh {arch} /target/os/{arch}/assembler"'),
            ("inputs", arr(["os/image/assembler.Containerfile", "os/image/assembler.sh", "tools/ci/publish/lib.sh",
                            "os/base-snapshot.env", "os/dev/resolve-rpm-snapshot.sh", "os/dev/image-epoch.sh"])),
            ("needs", arr([f"image-rpms-{arch}"])),
            ("outputs", arr([f"/target/os/{arch}/assembler/**"])),
            ("requires", req), ("isolation", '"vm"'), ("cost", '"minutes"'),
            ("cpus", "4"), ("memory", '"8G"'), ("default", "false")]))
        out.append(check([
            ("name", f'"os-base-{arch}"'),
            ("image", f'"{SKOPEO}"'),
            ("run", f'"bash os/image/build-layer.sh base {arch} /target/os/{arch}/base"'),
            ("inputs", arr(["os/base-snapshot.env", "os/image/build-layer.sh", "tools/ci/publish/lib.sh"])),
            ("outputs", arr([f"/target/os/{arch}/base/**"])),
            ("cost", '"minutes"'), ("default", "false")]))
        for img in plan.get("image", []):
            name = img["name"]
            files = sorted(set(tools + [f"os/image/helpers/{h}.Containerfile" for h in
                                        pieces.needed_helpers(pieces.action_text(name), set())]
                               + pieces.inputs(name, set())))
            out.append(check([
                ("name", f'"os-{name}-{arch}"'),
                ("image", ASSEMBLER_IMAGE.format(arch=arch)),
                ("run", f'"bash os/image/build-layer.sh image {name} {arch} /target/os/{arch}/{name}"'),
                ("inputs", arr(files)),
                ("needs", arr([f"os-base-{arch}", "os-assets", f"image-rpms-{arch}"])),
                ("outputs", arr([f"/target/os/{arch}/{name}/**"])),
                ("requires", req), ("isolation", '"vm"'), ("writable_sources", "true"),
                ("cost", '"heavy"'), ("cpus", "8"), ("memory", '"16G"'), ("timeout_secs", "7200"),
                ("default", "false")]))
        for variant in VARIANTS:
            suffix = "" if variant == "dev" else "-release"
            lto = "off" if variant == "dev" else "thin"
            out.append(check([
                ("name", f'"os-compile{suffix}-{arch}"'),
                ("image", BUILDER_IMAGE.format(arch=arch)),
                ("run", f'"bash os/image/compile.sh /target/os/{arch}/compiled{suffix}"'),
                ("inputs", arr(sorted(set(builder_sources + ["os/image/compile.sh"])))),
                ("needs", arr(["os-assets"])),
                ("env", f'{{ TILO_RUST_LTO = "{lto}", TILO_WITH_BROWSER = "1" }}'),
                ("outputs", arr([f"/target/os/{arch}/compiled{suffix}/**"])),
                ("requires", arr([node])),
                ("cost", '"heavy"'), ("cpus", "16"), ("memory", '"32G"'), ("timeout_secs", "10800"),
                ("default", "false")] + ([("on", RELEASE_ON)] if variant == "release" else [])))
        for variant in VARIANTS:
            below = []
            for l in plan["layer"]:
                name = l["name"]
                if variant == "release" and name in SHARED:
                    below.append(layer_check_name(name, arch, variant))
                    continue
                given = set(given_stages(l))
                if not l.get("scratch"):
                    given.add(pieces.parent(name))
                files = sorted(set(tools + piece_files(name, given) + pieces.inputs(name, given)))
                needs = [f"os-base-{arch}", "os-assets", f"image-rpms-{arch}"] + list(below)
                for g in l.get("given", []):
                    if g == "compiled":
                        needs.append(f"os-compile{'' if variant == 'dev' else '-release'}-{arch}")
                        continue
                    what = g.split(":", 1)[0]
                    if any(i["name"] == what for i in plan.get("image", [])):
                        needs.append(f"os-{what}-{arch}")
                    else:
                        needs.append(layer_check_name(what, arch, variant))
                needs = list(dict.fromkeys(needs))
                fields = [
                    ("name", f'"{layer_check_name(name, arch, variant)}"'),
                    ("image", ASSEMBLER_IMAGE.format(arch=arch)),
                    ("run", f'"bash os/image/build-layer.sh layer {name} {arch} {variant} {layer_out(name, arch, variant)}"'),
                    ("inputs", arr(files)),
                    ("needs", arr(needs)),
                    ("outputs", arr([f"{layer_out(name, arch, variant)}/**"])),
                    ("git", "true"),
                    ("requires", req), ("isolation", '"vm"'), ("writable_sources", "true"),
                    ("cost", '"heavy"'), ("cpus", "8"), ("memory", '"16G"'), ("timeout_secs", "7200"),
                    ("default", "false")]
                if variant == "release" and name not in SHARED:
                    fields.append(("on", RELEASE_ON))
                out.append(check(fields))
                below.append(layer_check_name(name, arch, variant))
            suffix = "" if variant == "dev" else "-release"
            fields = [
                ("name", f'"os-image{suffix}-{arch}"'),
                ("run", f'"bash os/image/build-layer.sh compose {arch} {variant} /target/os/{arch}/image{suffix}"'),
                ("inputs", arr(["os/image/build-layer.sh", "os/image/layers.toml", "tools/ci/os/oci_layers.py", "os/base-snapshot.env"])),
                ("needs", arr([f"os-base-{arch}"] + below)),
                ("outputs", arr([f"/target/os/{arch}/image{suffix}/**"])),
                ("cost", '"minutes"'), ("default", "false")]
            if variant == "release":
                fields.append(("on", RELEASE_ON))
            out.append(check(fields))
    for arch, node in ARCHES:
        out.append(check([
            ("name", f'"os-disk-{arch}"'),
            ("image", ASSEMBLER_IMAGE.format(arch=arch)),
            ("run", f'"bash os/image/build-layer.sh disk {arch} /target/os/{arch}/disk"'),
            ("inputs", arr(sorted(set(["os/image/build-layer.sh", "tools/ci/publish/lib.sh", "os/dev/build-image.sh",
                                       "os/dev/disk-image.sh", "os/account.env"] + scripts_of(["os/dev/disk-image.sh"]))))),
            ("needs", arr([f"os-image-{arch}"])),
            ("outputs", arr([f"/target/os/{arch}/disk/**"])),
            ("git", "true"),
            ("requires", arr([node, "disk-100g"])), ("isolation", '"vm"'), ("writable_sources", "true"),
            ("cost", '"heavy"'), ("cpus", "4"), ("memory", '"8G"'), ("timeout_secs", "3600"),
            ("default", "false")]))
    for variant in VARIANTS:
        suffix = "" if variant == "dev" else "-release"
        fields = [
            ("name", f'"os-image{suffix}"'),
            ("run", f'"python3 tools/ci/os/oci_layers.py index /target/os/image{suffix} ' +
             " ".join(f"/target/os/{a}/image{suffix}/oci" for a, _ in ARCHES) + '"'),
            ("inputs", arr(["tools/ci/os/oci_layers.py"])),
            ("needs", arr([f"os-image{suffix}-{a}" for a, _ in ARCHES])),
            ("outputs", arr([f"/target/os/image{suffix}/**"])),
            ("cost", '"seconds"'), ("default", "false")]
        if variant == "release":
            fields.append(("on", RELEASE_ON))
        out.append(check(fields))
    out += release_checks()
    return "\n".join(out)


SIGNING_ENV = ('{ COSIGN_KEY_REF = "awskms:///arn:aws:kms:us-east-1:125650147312:alias/tilo-release-signing", '
               'AWS_SIGNING_ROLE_ARN = "arn:aws:iam::125650147312:role/TiloTReleaseSigner", '
               'TILO_SIGNING_AUDIENCE = "sts.amazonaws.com" }')
RELEASE_REFS = '["stable", "tag:v[0-9]*.[0-9]*.[0-9]*"]'


def release_checks():
    """Stable's half: main's release image of the commit, sealed with the release keys and
    booted (the boot layer is the one layer main never tested), signed, and published by
    the server, which pushes only the blobs the registry lacks. And main's integration image,
    published nightly to the development repository."""
    seal_tools = sorted(set(["tools/ci/publish/os-release.sh", "tools/ci/publish/lib.sh", "tools/ci/os/oci_layers.py",
                             "os/dev/build-image.sh", "os/dev/vm.sh", "os/dev/disk-image.sh", "os/dev/reseal-image.sh",
                             "os/dev/sign-target-prediction.sh", "os/dev/configure-cosign-kms.sh", "keys/cosign.pub"]
                            + scripts_of(["os/dev/build-image.sh", "os/dev/vm.sh", "os/dev/disk-image.sh"])))
    out = []
    for arch, node in ARCHES:
        out.append(check([
            ("name", f'"os-release-{arch}"'),
            ("image", ASSEMBLER_IMAGE.format(arch=arch)),
            ("run", f'"bash tools/ci/publish/os-release.sh seal {arch} /target/os/{arch}/image-release /target/os/{arch}/sealed"'),
            ("inputs", arr(seal_tools)),
            ("needs", arr([f"os-image-release-{arch}"])),
            ("outputs", arr([f"/target/os/{arch}/sealed/**"])),
            ("secrets", arr(["TILO_PCR_PREDICTION_SIGNER", "TILO_EFI_SIGNING_CERT_SHA256"])),
            ("id_token", arr(["sts.amazonaws.com"])),
            ("env", SIGNING_ENV),
            ("git", "true"),
            ("requires", arr([node, "/dev/kvm", "disk-100g"])), ("isolation", '"vm"'), ("writable_sources", "true"),
            ("cost", '"heavy"'), ("cpus", "8"), ("memory", '"16G"'), ("timeout_secs", "7200"),
            ("on", RELEASE_REFS)]))
    out.append(check([
        ("name", '"sign-os"'),
        ("image", ASSEMBLER_IMAGE.format(arch="x86_64")),
        ("run", '"bash tools/ci/publish/os-release.sh sign /target/os/release ' +
         " ".join(f"/target/os/{a}/sealed/oci" for a, _ in ARCHES) + '"'),
        ("inputs", arr(["tools/ci/publish/os-release.sh", "tools/ci/publish/lib.sh", "tools/ci/os/oci_layers.py",
                        "os/dev/configure-cosign-kms.sh", "os/dev/install-cached-cosign.sh", "keys/cosign.pub"])),
        ("needs", arr([f"os-release-{a}" for a, _ in ARCHES])),
        ("outputs", arr(["/target/os/release/**"])),
        ("id_token", arr(["sts.amazonaws.com"])),
        ("env", SIGNING_ENV),
        ("requires", arr(["arch=x86_64"])),
        ("on", RELEASE_REFS)]))
    out.append(check([
        ("name", '"publish-os"'),
        ("run", '"echo \'publish-os is pushed by the t server from its store (publish in t.toml)\' >&2; exit 1"'),
        ("inputs", arr(["tools/ci/os/os_toml.py"])),
        ("needs", arr(["sign-os"])),
        ("id_token", arr(["registry.tilo.so"])),
        ("publish", '{ layout = "/target/os/release/oci", repository = "registry.tilo.so/tilo/tilo", tags = ["sha-{commit}"] }'),
        ("on", RELEASE_REFS)]))
    out.append(check([
        ("name", '"publish-os-ci"'),
        ("run", '"echo \'publish-os-ci is pushed by the t server from its store (publish in t.toml)\' >&2; exit 1"'),
        ("inputs", arr(["tools/ci/os/os_toml.py"])),
        ("needs", arr(["os-image"])),
        ("id_token", arr(["registry.tilo.so"])),
        ("publish", '{ layout = "/target/os/image/oci", repository = "registry.tilo.so/tilo/tilo-ci", tags = ["main-{commit}"] }'),
        ("on", '["schedule:30 0 * * *", "dispatch"]')]))
    return out


if __name__ == "__main__":
    sys.stdout.write(block())
