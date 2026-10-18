#!/usr/bin/env python3
"""The OS image's Containerfile pieces (os/image/), and os/Containerfile made of them.

    pieces.py write                  write os/Containerfile: the build arguments, every
                                     helper stage and every layer's piece, in the order
                                     os/image/layers.toml gives
    pieces.py check                  fail when os/Containerfile is not what `write` writes
    pieces.py containerfile ACTION [--given STAGE,...]
                                     the Containerfile one action builds: the arguments,
                                     the helper stages its piece needs (but those given to
                                     it as images) and the piece
    pieces.py parent ACTION          the stage the action builds over, as its piece names it
    pieces.py inputs ACTION [--given STAGE,...]
                                     the repository files the action's Containerfile reads
                                     (what it COPYs and bind-mounts from the context), as
                                     globs, for its key

Each layer of the image is one piece, which builds over the layers below it given as an
image; a helper stage is what pieces build on without shipping it (a builder, a
rendered asset, a pinned input). os/Containerfile is the same pieces in one file, so
one `podman build` makes the whole image, and the checks that read the image's recipe
read one file.
"""

import os
import re
import sys
import tomllib

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
IMAGE = os.path.join(REPO, "os", "image")
GENERATED = "# Written by `python3 tools/ci/os/pieces.py write` from os/image/: edit the pieces, not this file.\n"
FROM = re.compile(r"^FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?", re.M)
FROM_REF = re.compile(r"(?:--from=|\bfrom=)([A-Za-z0-9_${}.-]+)")


def plan():
    with open(os.path.join(IMAGE, "layers.toml"), "rb") as f:
        return tomllib.load(f)


def read(path):
    with open(path) as f:
        return f.read()


def helpers():
    """Helper name -> its text, in os/Containerfile's order."""
    return {h: read(os.path.join(IMAGE, "helpers", f"{h}.Containerfile")) for h in plan()["helpers"]}


def stages(text):
    """The stages a Containerfile text defines, with what each is FROM."""
    return {m.group(2): m.group(1) for m in FROM.finditer(text) if m.group(2)}


def references(text):
    """The stage names a text builds from or copies and mounts from."""
    refs = {m.group(1) for m in FROM.finditer(text)}
    refs |= {m.group(1) for m in FROM_REF.finditer(text)}
    return refs


def needed_helpers(text, given):
    """The helpers a piece needs, transitively, in order, leaving out stages given as images.
    A reference with a build argument in it (windows-provider-${TILO_WINDOWS_ARCH}-...) is to
    whichever stage of its helper the argument picks: the whole helper comes along."""
    hs = helpers()
    defines = {}
    for name, t in hs.items():
        for s in stages(t):
            defines[s] = name
    want, todo = set(), [text]
    own = set(stages(text))
    while todo:
        t = todo.pop()
        for ref in references(t):
            if ref in given or ref in own or ref == "scratch":
                continue
            if ref in defines:
                names = [defines[ref]]
            elif re.fullmatch(r"\$\{[^}]*\}", ref) or "${" not in ref:
                # An image reference by build argument (${BASE_REF}) or by name: no stage.
                names = []
            else:
                pattern = re.sub(r"\\\$\\\{[^}]*\\\}", ".*", re.escape(ref))
                names = [h for st, h in defines.items() if re.fullmatch(pattern, st)]
            for h in names:
                if h not in want:
                    want.add(h)
                    todo.append(hs[h])
    return [h for h in hs if h in want]


def layer(name):
    for l in plan()["layer"]:
        if l["name"] == name:
            return l
    sys.exit(f"pieces: no layer {name} in os/image/layers.toml")


def action_text(name):
    for i in plan().get("image", []):
        if i["name"] == name:
            # An image action builds helper stages: they are its piece.
            return "".join(helpers()[t] for t in needed_helpers("".join(f"FROM {t}\n" for t in i["targets"]), set()))
    return read(os.path.join(IMAGE, f"{layer(name)['piece']}.Containerfile"))


def containerfile(name, given):
    text = action_text(name)
    out = [read(os.path.join(IMAGE, "args.Containerfile"))]
    hs = helpers()
    for h in needed_helpers(text, given):
        if hs[h] not in text:
            out.append(hs[h])
    out.append(text)
    return "\n".join(t.rstrip("\n") + "\n" for t in out)


def parent(name):
    """The stage below the layer's target that its piece doesn't define: what it builds over."""
    l = layer(name)
    defined = stages(action_text(name))
    s = l["target"]
    while s in defined:
        s = defined[s]
    return s


def inputs(name, given):
    """Globs of the repository files the action's Containerfile reads from its context."""
    return inputs_of_text(containerfile(name, given))


def inputs_of_text(text):
    """Globs of the repository files a Containerfile text reads from its context."""
    found = set()
    for line in logical_lines(text):
        if line.startswith("COPY") and "--from=" not in line.split("#")[0]:
            words = [w for w in line.split()[1:] if not w.startswith("--")]
            found.update(words[:-1])
        for m in re.finditer(r"--mount=type=bind,([^ ]*)", line):
            opts = dict(o.split("=", 1) for o in m.group(1).split(",") if "=" in o)
            if "from" not in opts and "source" in opts:
                found.add(opts["source"])
    globs = set()
    for p in found:
        p = re.sub(r"\$\{[^}]*\}", "*", p).rstrip("/")
        full = os.path.join(REPO, p)
        globs.add(f"{p}/**" if os.path.isdir(full) or "*" in p else p)
    return sorted(globs)


def logical_lines(text):
    """Instructions with their continuation lines joined, comments left out."""
    out, cur = [], ""
    for raw in text.split("\n"):
        if raw.lstrip().startswith("#"):
            continue
        if raw.endswith("\\"):
            cur += raw[:-1] + " "
            continue
        cur += raw
        if cur.strip():
            out.append(cur.strip())
        cur = ""
    return out


def whole():
    p = plan()
    parts = [GENERATED + read(os.path.join(IMAGE, "args.Containerfile"))]
    parts += [helpers()[h] for h in p["helpers"]]
    parts += [read(os.path.join(IMAGE, f"{l['piece']}.Containerfile")) for l in p["layer"]]
    return "\n".join(t.rstrip("\n") + "\n" for t in parts)


def given_arg(argv):
    if "--given" in argv:
        i = argv.index("--given")
        return {s for s in argv[i + 1].split(",") if s}
    return set()


def main(argv):
    target = os.path.join(REPO, "os", "Containerfile")
    if argv[:1] == ["write"]:
        with open(target, "w") as f:
            f.write(whole())
    elif argv[:1] == ["check"]:
        if read(target) != whole():
            sys.exit("os/Containerfile is not os/image/'s pieces: run `python3 tools/ci/os/pieces.py write`")
    elif argv[:1] == ["containerfile"] and len(argv) >= 2:
        sys.stdout.write(containerfile(argv[1], given_arg(argv)))
    elif argv[:1] == ["parent"] and len(argv) == 2:
        print(parent(argv[1]))
    elif argv[:1] == ["inputs"] and len(argv) >= 2:
        print("\n".join(inputs(argv[1], given_arg(argv))))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv[1:])
