#!/usr/bin/env python3
"""The OS image's checks, written into t.toml from os/image/layers.toml.

    os_toml.py            print the block (tools/ci/t_toml.py writes it as `os-layers`)

Per architecture: the assembler (the image the actions run in), the base, the OS builder
and side builds, the compiles (in the OS builder, with t's compiler cache and warm build
directories: the image's, and the dev image's components), one check per layer, and the
composition, which is `os-image-<arch>`.

Each layer check is keyed by exactly what its piece reads: its Containerfile and the
helper stages it builds on, every file those COPY or bind-mount, the tools that build
and verify a layer, and the layers below it (its needs). So a changed unit file reruns
the system layer and what is above it, a changed crate the compile and the layers from
the binaries up, and neither ever the package transaction.
"""

import os
import re
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pieces  # noqa: E402

REPO = pieces.REPO
ARCHES = [("x86_64", "arch=x86_64"), ("aarch64", "arch=arm64")]
ASSEMBLER_IMAGE = '{{ check = "os-assembler-{arch}", path = "/target/os/{arch}/assembler/oci" }}'
BUILDER_IMAGE = '{{ check = "os-builder-{arch}", path = "/target/os/{arch}/builder/builder-base/oci" }}'
SKOPEO = "registry.tilo.so/tilo/skopeo@sha256:7e735527d9d64c7456fc82d7f954c380b4a5dd1a8173115ce2ed7b3442ef36e0"
# The tools every root action builds and verifies through.
TOOLS = ["os/image/build-layer.sh", "os/image/layers.toml", "os/image/args.Containerfile",
         "tools/ci/os/pieces.py", "tools/ci/os/oci_layers.py", "tools/ci/publish/lib.sh",
         "os/base-snapshot.env", "os/kernel-snapshot.env", "os/dev/build-image.sh", "keys/cosign.pub"]
# What measures a built image and judges it against os/image-size-budget.toml.
IMAGE_SIZE_TOOLS = ["tools/ci/os/image-size.sh", "tools/perf/image_size.py",
                    "os/tests/image_size_budget.py", "os/image-size-budget.toml"]


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
            with open(full, errors="replace") as f:
                text = f.read()
            for m in pat.finditer(text):
                q = "os/" + m.group(1)
                if q not in found and os.path.isfile(os.path.join(REPO, q)):
                    found.add(q)
                    nxt.append(q)
        todo = nxt
    return sorted(found)


# The verified, read-only fixture cache every build that ships takes (TILO_TEST_ASSET_SEED):
# filled once by the `test-assets` check, keyed by the manifests, so a trusted build, which
# reruns every build script, never reaches the forge for a fixture.
TEST_ASSET_SEED = "/target/test-assets"


def fixture_manifests():
    """Every test-asset manifest a build script names (tools/test-assets-build.rs)."""
    found = set()
    for pattern in ("apps/*/build.rs", "crates/*/*/build.rs"):
        for script in sorted(Path(REPO).glob(pattern)):
            text = script.read_text(encoding="utf-8")
            if "prepare_fixture_assets" not in text:
                continue
            for relative in re.findall(r'"([^"]*artifacts\.json)"', text):
                found.add(os.path.relpath(os.path.normpath(script.parent / relative), REPO))
    return sorted(found)


# The pinned Cosign of both architectures, downloaded once for every check that verifies or signs
# with it (tools/ci/publish/lib.sh use_cosign), apart from os-assets so a signer waits for no more.
SIGNING_TOOLS = "/target/signing-tools"
COSIGN_ASSETS = ["cosign-v2.5.2-linux-amd64", "cosign-v2.5.2-linux-arm64"]


def signing_tools_check():
    return check([("name", '"signing-tools"'),
                  ("run", f'"TILO_GITHUB_ASSET_CACHE={SIGNING_TOOLS} TILO_ASSET_STORE= bash os/dev/cache-github-assets.sh {" ".join(COSIGN_ASSETS)}"'),
                  ("inputs", arr(["os/external-assets/github-assets.tsv", "os/dev/cache-github-assets.sh"])),
                  ("outputs", arr([f"{SIGNING_TOOLS}/**"])),
                  ("requires", arr(["arch=any"])),
                  ("cost", '"minutes"'), ("requirement", requirement(1, "1GiB")), ("default", "false")])


def test_assets_check():
    manifests = fixture_manifests()
    return check([("name", '"test-assets"'),
                  ("run", f'"python3 tools/test-assets.py --cache-root {TEST_ASSET_SEED} {" ".join(manifests)}"'),
                  ("inputs", arr(["tools/test-assets.py", *manifests])),
                  ("outputs", arr([f"{TEST_ASSET_SEED}/**"])),
                  ("requires", arr(["arch=any"])),
                  ("cost", '"minutes"'), ("requirement", requirement(1, "1GiB")), ("default", "false")])


def arr(items):
    return "[" + ", ".join(f'"{i}"' for i in items) + "]"


def check(fields):
    out = ["[[check]]"]
    for k, v in fields:
        out.append(f"{k} = {v}")
    return "\n".join(out) + "\n"


def requirement(cores, memory):
    return f'{{ cores = {cores}, memory = "{memory}" }}'


def piece_files(name, given):
    """The piece's own file and the helper files it builds on."""
    text = pieces.action_text(name)
    files = [f"os/image/{piece}.Containerfile" for piece in pieces.piece_names(pieces.layer(name))]
    for h in pieces.needed_helpers(text, given):
        files.append(f"os/image/helpers/{h}.Containerfile")
    return files


def given_stages(layer):
    return [g.split(":", 1)[1] for g in layer.get("given", []) if g != "compiled"]


def block():
    plan = pieces.plan()
    tools = TOOLS + scripts_of(TOOLS)
    out = ["""# The OS image as layer actions (os/image/layers.toml, os/image/build-layer.sh): per
# architecture the assembler the actions run in, the pinned base, the OS builder and the side
# builds, the compile in the OS builder with t's compiler cache and warm build directories, one
# check per layer, each keyed by exactly what its piece reads, and the composition,
# os-image-<arch>, which writes a manifest over the layers' blobs in seconds. A changed crate
# reruns the compile, the binaries' layers and the derived and boot layers above them, never the
# packages, kernel or system layers. `t test os-image-x86_64` builds what a development VM runs
# from your tree (no LTO, the apps and desktop of the commit, its UKI sealing a placeholder
# composefs digest) and leaves it as an OCI layout (`t outputs`).
""",
           check([("name", '"os-assets"'),
                  ("run", '"bash os/image/build-layer.sh assets"'),
                  ("inputs", arr(["os/external-assets/github-assets.tsv", "os/dev/cache-github-assets.sh", "os/image/build-layer.sh"])),
                  ("outputs", arr(["os/external-assets/cache/**"])),
                  ("cost", '"minutes"'), ("default", "false")]),
           signing_tools_check(),
           test_assets_check()]
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
            ("requirement", requirement(4, "8GiB")), ("default", "false")]))
        out.append(check([
            ("name", f'"os-base-{arch}"'),
            ("image", f'"{SKOPEO}"'),
            ("run", f'"bash os/image/build-layer.sh base {arch} /target/os/{arch}/base"'),
            ("inputs", arr(["os/base-snapshot.env", "os/image/build-layer.sh", "tools/ci/publish/lib.sh"])),
            ("outputs", arr([f"/target/os/{arch}/base/**"])),
            ("requires", arr(["arch=any"])),
            ("cost", '"minutes"'), ("requirement", requirement(1, "2GiB")), ("default", "false")]))
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
                ("needs", arr([f"os-base-{arch}", "os-assets", "signing-tools", f"image-rpms-{arch}"])),
                ("outputs", arr([f"/target/os/{arch}/{name}/**"])),
                ("requires", req), ("isolation", '"vm"'), ("writable_sources", "true"),
                ("cost", '"heavy"'), ("requirement", requirement(8, "16GiB")), ("timeout_secs", "7200"),
                ("default", "false")]))
        out.append(check([
            ("name", f'"os-compile-{arch}"'),
            ("image", BUILDER_IMAGE.format(arch=arch)),
            ("run", f'"bash os/image/compile.sh /target/os/{arch}/compiled"'),
            ("inputs", arr(sorted(set(builder_sources + ["os/image/compile.sh"])))),
            ("needs", arr(["os-assets", "test-assets"])),
            # The development image's compile: no LTO and no line tables, as Build / OS dev did.
            # A build to ship, not a lint gate: warnings stay warnings, as in every image build.
            ("env", '{ TILO_RUST_LTO = "off", TILO_RUST_DEBUGINFO = "0", TILO_WITH_BROWSER = "1", '
                    f'TILO_TEST_ASSET_SEED = "{TEST_ASSET_SEED}", '
                    'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "", '
                    'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "" }'),
            ("outputs", arr([f"/target/os/{arch}/compiled/**"])),
            ("requires", arr([node])),
            ("cost", '"heavy"'), ("requirement", requirement(16, "32GiB")), ("timeout_secs", "10800"),
            ("default", "false")]))
        out.append(check([
            ("name", f'"os-compile-dev-{arch}"'),
            ("image", BUILDER_IMAGE.format(arch=arch)),
            ("run", f'"bash os/image/compile.sh --dev /target/os/{arch}/compiled-dev"'),
            ("inputs", arr(sorted(set(builder_sources + ["os/image/compile.sh"])))),
            ("needs", arr(["test-assets"])),
            # The dev components of the development image: no LTO, as Build / OS dev built them.
            ("env", '{ TILO_DEV_RUST_LTO = "off", '
                    f'TILO_TEST_ASSET_SEED = "{TEST_ASSET_SEED}", '
                    'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "", '
                    'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "" }'),
            ("outputs", arr([f"/target/os/{arch}/compiled-dev/**"])),
            ("requires", arr([node])),
            ("cost", '"heavy"'), ("requirement", requirement(16, "32GiB")), ("timeout_secs", "10800"),
            ("default", "false")]))
        below = []
        for l in plan["layer"]:
            name = l["name"]
            given = set(given_stages(l))
            if not l.get("scratch"):
                given.add(pieces.parent(name))
            files = sorted(set(tools + piece_files(name, given) + pieces.inputs(name, given)))
            needs = [f"os-base-{arch}", "os-assets", "signing-tools", f"image-rpms-{arch}"] + below
            for g in l.get("given", []):
                if g == "compiled":
                    needs.append(f"os-compile-{arch}")
                    continue
                what = g.split(":", 1)[0]
                if any(i["name"] == what for i in plan.get("image", [])):
                    needs.append(f"os-{what}-{arch}")
                else:
                    needs.append(f"os-layer-{what}-{arch}")
            out.append(check([
                ("name", f'"os-layer-{name}-{arch}"'),
                ("image", ASSEMBLER_IMAGE.format(arch=arch)),
                ("run", f'"bash os/image/build-layer.sh layer {name} {arch} /target/os/{arch}/layers/{name}"'),
                ("inputs", arr(files)),
                ("needs", arr(list(dict.fromkeys(needs)))),
                ("outputs", arr([f"/target/os/{arch}/layers/{name}/**"])),
                # No commit: the clock is the base's and the RPM snapshot's, the build context
                # leaves .git out and the generation comes from the published series, so the
                # files a layer reads and the layers below key it and an unchanged layer is
                # reused on every later commit. A development image reusing the image layer has
                # the same binaries, which need no newer generation to be told apart.
                ("requires", req), ("isolation", '"vm"'), ("writable_sources", "true"),
                ("cost", '"heavy"'), ("requirement", requirement(8, "16GiB")), ("timeout_secs", "7200"),
                ("default", "false")]))
            below.append(f"os-layer-{name}-{arch}")
        out.append(check([
            ("name", f'"os-image-{arch}"'),
            ("run", f'"bash os/image/build-layer.sh compose {arch} /target/os/{arch}/image"'),
            ("inputs", arr(["os/image/build-layer.sh", "os/image/layers.toml", "tools/ci/os/oci_layers.py", "tools/ci/publish/lib.sh"])),
            ("needs", arr([f"os-base-{arch}"] + below)),
            ("outputs", arr([f"/target/os/{arch}/image/**"])),
            ("requires", arr(["arch=any"])),
            ("cost", '"minutes"'), ("requirement", requirement(1, "2GiB")), ("default", "false")]))
        # The composed image's /usr against its budget: every check that ships or boots
        # os-image-<arch> needs this one, so an image over budget goes nowhere.
        out.append(check([
            ("name", f'"os-image-size-{arch}"'),
            ("run", f'"bash tools/ci/os/image-size.sh production-{arch} /target/os/{arch}/image/oci '
                    f'/target/os/{arch}/size"'),
            ("inputs", arr(IMAGE_SIZE_TOOLS)),
            ("needs", arr([f"os-image-{arch}"])),
            ("outputs", arr([f"/target/os/{arch}/size/**"])),
            ("requires", arr(["arch=any"])),
            ("cost", '"minutes"'), ("requirement", requirement(2, "4GiB")), ("default", "false")]))
    return "\n".join(out)


if __name__ == "__main__":
    sys.stdout.write(block())
