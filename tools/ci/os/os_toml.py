#!/usr/bin/env python3
"""The OS image's checks, written into t.toml from os/image/layers.toml.

    os_toml.py            print the block (tools/ci/t_toml.py writes it as `os-layers`)

Per architecture: the assembler (the image the actions run in), the base, the OS builder
and side builds, the compiles (in the OS builder, with t's compiler cache and warm build
directories: the image's, and the dev image's components), one check per layer, and the
composition, which is `os-image-<arch>`; then the release image's compile (thin LTO), its
`release` layers and its composition, `os-release-image-<arch>` (tools/ci/publish/os-release.sh
seals, signs and publishes it); and for each board of layers.toml's `release_boards` on that
architecture, its `board` layers and its composition, `os-release-image-<board>`.

Each layer check is keyed by exactly what its piece reads: its Containerfile and the
helper stages it builds on, every file those COPY or bind-mount, the tools that build
and verify a layer, and the layers below it (its needs). So a changed unit file reruns
the system layer and what is above it, a changed crate the compile and the layers from
the binaries up, and neither ever the package transaction.
"""

import io
import json
import os
import re
import sys
import tokenize
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
         "os/base-snapshot.env", "os/dev/build-image.sh", "keys/cosign.pub"]
# The kernel pin and what verifies it, which only an action whose Containerfile builds FROM
# the pinned kernel reads: build-image.sh resolves and verifies the kernel for no other
# (TILO_LAYER_TARGET). So a new kernel pin reruns the kernel layer and the layers above it,
# never the OS builder, the side builds, the compiles or the package transaction.
KERNEL_PIN = "os/kernel-snapshot.env"
KERNEL_VERIFIER = "os/dev/verify-kernel-provenance.sh"
KERNEL_FROM = "FROM ${KERNEL_RPMS_REF}"
# What the browser's engine is built from (os/image/compile.sh --chromium): its host's sources,
# the build script and the CEF pin, and the blocker crate it links with what cargo reads to
# build that.
CHROMIUM_SOURCES = ["os/image/compile.sh", "scripts/browser/build-host.py", "scripts/browser/cef-lock.json",
                    "apps/browser/chromium/**", "crates/browser/chromium-blocker/**", "crates/workspace-hack/**",
                    "vendor/**", "Cargo.lock", "**/Cargo.toml", ".cargo/**", "rust-toolchain.toml",
                    "os/external-assets/github-assets.tsv"]
# What measures a built image and judges it against os/image-size-budget.toml.
IMAGE_SIZE_TOOLS = ["tools/ci/os/image-size.sh", "tools/perf/image_size.py",
                    "os/tests/image_size_budget.py", "os/image-size-budget.toml"]


# A script names another by its repository path, or one beside it by its bare name: quoted,
# a word, or after its own directory ("$(dirname "${BASH_SOURCE[0]}")/x.sh", "$HERE/x.sh").
SCRIPT = re.compile(r"(?:\$TILO_OS_DIR|\$\{TILO_OS_DIR\}|os)/((?:dev|tests|scripts)/[A-Za-z0-9_.-]+\.(?:sh|py))")
SIBLING = re.compile(r"""(?:^|[\s"'=(]|(?:\)|\}|\$\w+)/)([A-Za-z0-9_][A-Za-z0-9_.-]*\.(?:sh|py))\b""", re.M)


def without_comments(path, text):
    """A file's text without its comments, so a script named only in prose keys nothing. Python's
    comments and docstrings go by its tokenizer; every other file's (shell, TOML, env,
    Containerfile) from a `#` that starts a word outside quotes to the end of the line."""
    if path.endswith(".py"):
        toks = [t for t in tokenize.generate_tokens(io.StringIO(text).readline)
                if t.type not in (tokenize.COMMENT, tokenize.NL)]
        starts = (tokenize.INDENT, tokenize.DEDENT, tokenize.NEWLINE)
        return " ".join(t.string for i, t in enumerate(toks)
                        if not (t.type == tokenize.STRING and (i == 0 or toks[i - 1].type in starts)
                                and i + 1 < len(toks) and toks[i + 1].type == tokenize.NEWLINE))
    lines = []
    for line in text.splitlines():
        quote, escaped, cut = None, False, len(line)
        for i, c in enumerate(line):
            if escaped:
                escaped = False
            elif c == "\\" and quote != "'":
                escaped = True
            elif quote:
                quote = None if c == quote else quote
            elif c in "'\"":
                quote = c
            elif c == "#" and (i == 0 or line[i - 1].isspace()):
                cut = i
                break
        lines.append(line[:cut])
    return "\n".join(lines)


def scripts_of(paths, skip=()):
    """The os/ scripts the given scripts run, transitively: build-image.sh resolves and
    verifies every pin through them, so they key every layer. A script in `skip` is left out
    with what only it runs. Only code names a script; a comment naming one keys nothing."""
    found, todo = set(skip) | set(paths), list(paths)
    while todo:
        p = todo.pop()
        full = os.path.join(REPO, p)
        if not os.path.isfile(full):
            continue
        with open(full, errors="replace") as f:
            text = without_comments(p, f.read())
        named = ["os/" + m.group(1) for m in SCRIPT.finditer(text)]
        named += [os.path.join(os.path.dirname(p), m.group(1)) for m in SIBLING.finditer(text)]
        for q in named:
            if q not in found and os.path.isfile(os.path.join(REPO, q)):
                found.add(q)
                todo.append(q)
    return sorted(found - set(skip) - set(paths))


def kernel_tools():
    """What keys an action that installs the pinned kernel, beyond TOOLS."""
    return [KERNEL_PIN, KERNEL_VERIFIER] + scripts_of([KERNEL_VERIFIER])


def reads_kernel(containerfile):
    """Whether an action's Containerfile builds FROM the pinned kernel."""
    return KERNEL_FROM in containerfile


def reads_generation(text):
    """Whether a piece bakes in the generation build-layer.sh asks the registry for."""
    return re.search(r"^ARG TILO_GENERATION\b", text, re.M) is not None


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


def board_arch(board):
    with open(os.path.join(REPO, "os/hardware/profiles", f"{board}.json"), encoding="utf-8") as f:
        return json.load(f)["architecture"]


def flavour_checks(plan, arch, req, tools, flavour, layer):
    """The layer checks of one flavour of the image (`dev`, `release` or a board's release), its
    composition and its size check. `layer` maps each layer to the check that built it for the
    flavour before, and is updated to this one's."""
    board = flavour if flavour not in ("dev", "release") else None
    mark = "board" if board else flavour
    if flavour == "dev":
        suffix, dirs, compiled, env, name = "", "layers", "os-compile", None, "os-image"
    elif flavour == "release":
        suffix, dirs, compiled, name = "-release", "release-layers", "os-compile-release", "os-release-image"
        env = '{ TILO_OS_FLAVOUR = "release" }'
    else:
        suffix, dirs, compiled, name = f"-release-{board}", f"board-layers/{board}", "os-compile-release", "os-release-image"
        env = f'{{ TILO_OS_FLAVOUR = "release", TILO_OS_BOARD = "{board}" }}'
    # A board's checks are named for the board, which says the architecture.
    tail = board or arch
    out, below, own = [], [], False
    for l in plan["layer"]:
        lname = l["name"]
        if flavour != "dev" and not l.get(mark):
            if own:
                sys.exit(f"os/image/layers.toml: {lname} is above a {mark} layer, so must be one")
            below.append(layer[lname])
            continue
        own = True
        given = set(given_stages(l))
        if not l.get("scratch"):
            given.add(pieces.parent(lname))
        extra = []
        if flavour != "dev":
            extra.append("os/publication.env")
        if board:
            extra.append("os/hardware/**")
        if reads_kernel(pieces.containerfile(lname, given)):
            extra += kernel_tools()
        files = sorted(set(tools + piece_files(lname, given) + pieces.inputs(lname, given) + extra))
        needs = [f"os-base-{arch}", "os-assets", "signing-tools", f"image-rpms-{arch}"] + below
        for g in l.get("given", []):
            if g == "compiled":
                # With the browser's engine, which the development compile leaves out.
                needs += [f"{compiled}-{arch}", f"os-chromium-{arch}"]
                continue
            what = g.split(":", 1)[0]
            if any(i["name"] == what for i in plan.get("image", [])):
                needs.append(f"os-{what}-{arch}")
            else:
                needs.append(layer[what])
        fields = [
            ("name", f'"os-layer-{lname}{suffix if board else suffix + "-" + arch}"'),
            ("image", ASSEMBLER_IMAGE.format(arch=arch)),
            ("run", f'"bash os/image/build-layer.sh layer {lname} {arch} /target/os/{arch}/{dirs}/{lname}"'),
            ("inputs", arr(files)),
            ("needs", arr(list(dict.fromkeys(needs)))),
            ("outputs", arr([f"/target/os/{arch}/{dirs}/{lname}/**"]))]
        if env:
            fields.append(("env", env))
        # A layer reads no commit (the clock is the base's and the snapshot's, the build context
        # leaves .git out), so its files and the layers below key it and an unchanged layer is
        # reused on every later commit. A board's layer stays per commit: its update origin names
        # the commit. So does a release's piece that bakes in the generation, which
        # build-layer.sh allocates from the published series rather than from any file: a release
        # must carry the series' next one when it is built. A development image's generation is
        # read when its layer is built; a later commit reusing the layer has the same binaries,
        # the same image, which needs no newer generation to be told apart.
        if board or (flavour != "dev" and reads_generation(pieces.action_text(lname))):
            fields.append(("git", "true"))
        out.append(check(fields + [
            ("requires", req), ("isolation", '"vm"'), ("writable_sources", "true"),
            ("cost", '"heavy"'), ("requirement", requirement(8, "16GiB")), ("timeout_secs", "7200"),
            ("default", "false")]))
        layer[lname] = f"os-layer-{lname}{suffix if board else suffix + '-' + arch}"
        below.append(layer[lname])
    image_dir = name.removeprefix("os-") + (f"-{board}" if board else "")
    out.append(check([
        ("name", f'"{name}-{tail}"'),
        ("run", f'"bash os/image/build-layer.sh compose {arch} /target/os/{arch}/{image_dir}"'),
        ("inputs", arr(["os/image/build-layer.sh", "os/image/layers.toml", "tools/ci/os/oci_layers.py", "tools/ci/publish/lib.sh"])),
        ("needs", arr([f"os-base-{arch}"] + below)),
        ("outputs", arr([f"/target/os/{arch}/{image_dir}/**"])),
        *([("env", env)] if env else []),
        ("requires", arr(["arch=any"])),
        ("cost", '"minutes"'), ("requirement", requirement(1, "2GiB")), ("default", "false")]))
    # The composed image's /usr against its budget: every check that ships or boots the image
    # needs this one, so an image over budget goes nowhere. A board's is held to the board's.
    size = {"dev": "size", "release": "release-size"}.get(flavour, f"release-size-{board}")
    budget = f"board-{board}" if board else f"production-{arch}"
    out.append(check([
        ("name", f'"{name}-size-{tail}"'),
        ("run", f'"bash tools/ci/os/image-size.sh {budget} /target/os/{arch}/{image_dir}/oci '
                f'/target/os/{arch}/{size}"'),
        ("inputs", arr(IMAGE_SIZE_TOOLS)),
        ("needs", arr([f"{name}-{tail}"])),
        ("outputs", arr([f"/target/os/{arch}/{size}/**"])),
        ("requires", arr(["arch=any"])),
        ("cost", '"minutes"'), ("requirement", requirement(2, "4GiB")), ("default", "false")]))
    return out


def block():
    plan = pieces.plan()
    tools = TOOLS + scripts_of(TOOLS, skip=[KERNEL_VERIFIER])
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
           # Fetched by the cache script itself, not through build-layer.sh: a change to how
           # layers are built would otherwise fetch them again, and rebuild the RPM snapshot,
           # the assembler and every image above them, which take these as needs.
           check([("name", '"os-assets"'),
                  ("run", '"bash os/dev/cache-github-assets.sh"'),
                  ("inputs", arr(["os/external-assets/github-assets.tsv", "os/dev/cache-github-assets.sh"])),
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
                               + pieces.inputs(name, set())
                               + (kernel_tools() if reads_kernel(pieces.containerfile(name, set())) else [])))
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
        # The browser's engine, which reads its own sources and the blocker crate it links, not
        # the workspace's: the compiles take it as it is, so a changed crate never builds it again.
        out.append(check([
            ("name", f'"os-chromium-{arch}"'),
            ("image", BUILDER_IMAGE.format(arch=arch)),
            ("run", f'"bash os/image/compile.sh --chromium /target/os/{arch}/chromium"'),
            ("inputs", arr(CHROMIUM_SOURCES)),
            ("needs", arr(["os-assets"])),
            ("env", '{ CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "", '
                    'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "" }'),
            ("outputs", arr([f"/target/os/{arch}/chromium/**"])),
            ("requires", arr([node])),
            ("cost", '"heavy"'), ("requirement", requirement(8, "16GiB")), ("timeout_secs", "10800"),
            ("default", "false")]))
        # The development image's compile: no LTO and no line tables. A build to ship, not a
        # lint gate: warnings stay warnings, as in every image build. It needs the verified
        # fixtures, and not the browser's engine, which the image layer takes from
        # os-chromium-<arch> itself: it starts once the OS builder is there, not after the
        # engine's own build. A release's: thin LTO, whose links of the large binaries take up
        # to 8 GiB each, so 8 at a time, with the engine in its tree.
        for suffix, lto, debuginfo, memory, jobs in (("", "off", "0", "32GiB", ""),
                                                     ("-release", "thin", "line-tables-only", "64GiB", "8")):
            release = bool(suffix)
            out.append(check([
                ("name", f'"os-compile{suffix}-{arch}"'),
                ("image", BUILDER_IMAGE.format(arch=arch)),
                ("run", f'"bash os/image/compile.sh /target/os/{arch}/compiled{suffix}"'),
                ("inputs", arr(sorted(set(builder_sources + ["os/image/compile.sh"])))),
                ("needs", arr(["test-assets"] + ([f"os-chromium-{arch}"] if release else []))),
                ("env", f'{{ TILO_RUST_LTO = "{lto}", TILO_RUST_DEBUGINFO = "{debuginfo}", TILO_WITH_BROWSER = "1", '
                        + (f'TILO_CHROMIUM_ENGINE = "/target/os/{arch}/chromium", ' if release
                           else 'TILO_CHROMIUM_ENGINE = "apart", ')
                        + (f'TILO_CARGO_BUILD_JOBS = "{jobs}", ' if jobs else "")
                        + f'TILO_TEST_ASSET_SEED = "{TEST_ASSET_SEED}", '
                        'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "", '
                        'CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS = "" }'),
                ("outputs", arr([f"/target/os/{arch}/compiled{suffix}/**"])),
                ("requires", arr([node])),
                ("cost", '"heavy"'), ("requirement", requirement(16, memory)), ("timeout_secs", "10800"),
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
        # The development image's layers, then the release image's (TILO_OS_FLAVOUR=release):
        # its `release` layers again, over the development image's below them; then each board
        # release's of this architecture (TILO_OS_BOARD): its `board` layers again, over the
        # release image's below them.
        layer = {}
        for flavour in ["dev", "release"] + [b for b in plan.get("release_boards", []) if board_arch(b) == arch]:
            out += flavour_checks(plan, arch, req, tools, flavour, layer)
    return "\n".join(out)


if __name__ == "__main__":
    sys.stdout.write(block())
