"""Which way a working tree's change reaches a VM: as userspace layers swapped over main's
published dev disk, or only through the full dev disk of its own commit.

    layer_route.py --metadata <cargo metadata json> PATH...

prints one line: `layers <component>,...` (possibly none: a change to the suite alone) or
`full <why>`. The rule is an allowlist. A path takes the layer route only when it is the
acceptance suite, an app's directory, or a file of a workspace package that only
os/components.toml userspace components build; anything else (the kernel, packages, the
image recipe, boot components, tilo-vmd, Cargo.lock, a path this rule does not know) goes the
full route, so the base disk never stands in for something the change touched.
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
# Paths read by the suite or the harness on the host only: nothing in the VM changes.
SUITE = ("scripts/vm-acceptance/", "tools/ci/vm-acceptance.sh", "tools/perf/",
         "tools/atspi_external_client.py")
# Packages the base disk carries compiled: the dev toolchain's tilo-vmd and its admin tool.
BASE_PACKAGES = {"tilo-vmd", "tilo-device-admin"}


def route(paths: list[str], metadata: dict, manifest: dict) -> tuple[str, list[str] | str]:
    members = {p["id"]: p for p in metadata["packages"] if p["id"] in set(metadata["workspace_members"])}
    root = Path(metadata["workspace_root"])
    dirs = sorted(((Path(p["manifest_path"]).parent.relative_to(root).as_posix() + "/", p["name"])
                   for p in members.values()), key=lambda item: -len(item[0]))
    # Reverse dependencies over the resolved graph: which workspace packages a package reaches.
    users: dict[str, set[str]] = {}
    for node in (metadata.get("resolve") or {}).get("nodes", []):
        for dep in node.get("deps", []):
            users.setdefault(dep["pkg"], set()).add(node["id"])
    name_of = {pid: p["name"] for pid, p in members.items()}
    id_of = {p["name"]: pid for pid, p in members.items()}

    def reached(name: str) -> set[str]:
        seen, todo = set(), [id_of[name]]
        while todo:
            pid = todo.pop()
            if pid in seen:
                continue
            seen.add(pid)
            todo += users.get(pid, ())
        return {name_of[p] for p in seen if p in name_of}

    userspace, other = {}, set()
    for c in manifest["component"]:
        if c.get("kind") == "userspace":
            for package in c.get("packages", []):
                userspace.setdefault(package, set()).add(c["name"])
        else:
            other |= set(c.get("packages", []))
    other |= BASE_PACKAGES

    swaps: set[str] = set()
    for path in paths:
        if path.startswith(SUITE):
            continue
        if path.startswith("apps/") and path.count("/") >= 2:
            app_dir = path.split("/")[1]
            owner = next((name for d, name in dirs if path.startswith(d)), None)
            if owner is None or not (reached(owner) - {owner}):
                swaps.add(f"app:{app_dir}")
                continue
        owner = next((name for d, name in dirs if path.startswith(d)), None)
        if owner is None:
            return "full", f"{path} is not in a workspace package or app this route knows"
        hit = reached(owner)
        base = sorted(hit & other)
        if base:
            return "full", f"{path} changes {owner}, which the base disk carries compiled ({', '.join(base[:4])})"
        components = set().union(*(userspace.get(p, set()) for p in hit))
        if not components:
            return "full", f"{path} changes {owner}, which no userspace component builds"
        swaps |= components
    return "layers", sorted(swaps)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--metadata", type=Path, required=True)
    ap.add_argument("paths", nargs="*")
    args = ap.parse_args()
    manifest = tomllib.loads((REPO / "os/components.toml").read_text(encoding="utf-8"))
    way, what = route(args.paths, json.loads(args.metadata.read_text()), manifest)
    print(f"{way} {','.join(what) if way == 'layers' else what}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
