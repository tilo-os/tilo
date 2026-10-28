#!/usr/bin/env python3
"""Whether each workspace member builds as one unit whatever `-p` selection pulls it in.

Cargo resolves features per selection: a member reached through `-p a` can get other features,
or dependencies with other features, than through `-p b`. Each such variant is a different
unit (`-C metadata`), compiled again by every job that selects it and never shared through the
compiler cache. This plans every member's selection (`cargo test --no-run -p <member> -p
workspace-unify`, as the cargo families select it, `--unit-graph`) and the whole workspace's,
and names each member whose unit in a selection differs from its unit in the workspace, with the
features that differ. A unit is named by its dependencies' units too: without `--target`, one
built for the host and one for the target can read alike but for what they depend on.

Comparing each selection with the workspace's is enough: features only grow as a selection
grows, so a member that builds alike alone and in the whole workspace builds alike in every
selection between.

Exit 1 when a member varies; `--report` prints every variant without failing.
"""

import argparse
import concurrent.futures
import hashlib
import json
import re
import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "ci"))
import package_features  # noqa: E402
import workspace_unify  # noqa: E402


def package_of(package_id: str) -> tuple[str, str, bool]:
    """(name, version, from a path) of a cargo package id, `path+file:///…/x#name@1.0` or
    `registry+…#name@1.0`, or the older `path+file:///…/name#1.0`."""
    source, _, rest = package_id.partition("#")
    if "@" in rest:
        name, _, version = rest.rpartition("@")
    else:
        name, version = source.rstrip("/").rsplit("/", 1)[-1], rest
    return name, version, source.startswith("path+")


def graph(selection: list[str]) -> dict[str, tuple[str, frozenset[str], tuple[str, ...]]]:
    """Each unit cargo would build for `selection`'s tests (`cargo test --no-run`, as the test
    and lint families select), exactly as cargo plans it (`--unit-graph`): (its package, its
    features, the units it depends on), by an id naming its package, target, mode and platform.
    What cargo hashes into a unit's `-C metadata` is in it: features, the dependencies' units,
    the profile and whether it is built for the host."""
    out = subprocess.run(
        ["cargo", "test", "--no-run", "--offline", *selection, "--unit-graph", "-Z", "unstable-options"],
        cwd=ROOT, check=True, capture_output=True, text=True,
        env={**os.environ, "RUSTC_BOOTSTRAP": "1"},
    ).stdout
    units = json.loads(out)["units"]
    names = []
    for u in units:
        name, version, path = package_of(u["pkg_id"])
        target = u["target"]
        names.append(f"{name} {version}{' (path)' if path else ''}|{target['name']}|{'/'.join(target['kind'])}"
                     f"|{u['mode']}|{u.get('platform') or 'target'}|{json.dumps(u['profile'], sort_keys=True)}"
                     f"|{','.join(sorted(u['features']))}")
    # Without --target, a package built alike for the host and the target is two units that
    # read alike and differ only in what they depend on: name each by its dependencies too.
    ids: dict[int, str] = {}

    def ident(k: int) -> str:
        if k not in ids:
            below = sorted(ident(d["index"]) for d in units[k]["dependencies"])
            ids[k] = f"{names[k]}|{hashlib.sha256(json.dumps(below).encode()).hexdigest()[:12]}"
        return ids[k]

    sys.setrecursionlimit(100000)
    return {ident(k): (names[k].split("|", 1)[0], frozenset(u["features"]),
                       tuple(ident(d["index"]) for d in u["dependencies"]))
            for k, u in enumerate(units)}


def identities(graph) -> dict[str, str]:
    """A hash of each unit and its dependencies' identities: what cargo's `-C metadata` of the
    unit depends on."""
    memo: dict[str, str] = {}

    def ident(n: str) -> str:
        if n not in memo:
            kids = sorted(ident(c) for c in graph[n][2])
            memo[n] = hashlib.sha256(json.dumps([n, kids]).encode()).hexdigest()[:16]
        return memo[n]

    return {n: ident(n) for n in graph}


def members() -> list[tuple[str, str]]:
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    ids = set(meta["workspace_members"])
    return sorted((p["name"], p["version"]) for p in meta["packages"] if p["id"] in ids)


def is_library(unit: str) -> bool:
    """A library unit as other packages use it (built, not a test of it)."""
    _, _, kinds, mode = unit.split("|")[:4]
    return mode == "build" and any(k in ("lib", "rlib", "proc-macro") for k in kinds.split("/"))


def member_units(graph, names: set[str]) -> dict[str, dict[str, frozenset[str]]]:
    """For each member in the graph: its unit identities, each with the member's features."""
    ids = identities(graph)
    out: dict[str, dict[str, frozenset[str]]] = {}
    for n, (pkg, feats, _) in graph.items():
        name = pkg.split(" ", 1)[0]
        if name in names and pkg.endswith("(path)") and is_library(n):
            out.setdefault(name, {})[ids[n]] = feats
    return out


def feature_sets(a, b, member: str) -> list[str]:
    """The units below `member`'s library in graph `a` and in graph `b` whose feature sets differ:
    for each package, target and mode, the feature sets only one of them builds it with."""
    def below(g):
        roots = [n for n, (pkg, _, _) in g.items()
                 if pkg.split(" ", 1)[0] == member and pkg.endswith("(path)") and is_library(n)]
        seen, todo = set(), list(roots)
        while todo:
            n = todo.pop()
            if n not in seen:
                seen.add(n)
                todo.extend(g[n][2])
        sets: dict[str, set[frozenset[str]]] = {}
        for n in seen:
            name, target, _, mode = n.split("|")[:4]
            sets.setdefault(f"{name} {target} {mode}", set()).add(g[n][1])
        return sets
    sa, sb = below(a), below(b)
    out = []
    for key in sorted(set(sa) | set(sb)):
        only_a, only_b = sa.get(key, set()) - sb.get(key, set()), sb.get(key, set()) - sa.get(key, set())
        if only_a or only_b:
            common = set.intersection(*map(set, (sa.get(key, set()) | sb.get(key, set())))) if (sa.get(key) or sb.get(key)) else set()
            show = lambda fs: sorted(sorted(f - common) for f in fs)
            out.append(f"{key}: only in A {show(only_a)}, only in B {show(only_b)}")
    return out


def explain(a, b, member: str) -> list[str]:
    """Where `member`'s unit in graph `a` first differs from its unit in graph `b`: each package
    whose own features differ, or whose dependencies differ by package, with nothing deeper
    differing to account for it."""
    ia, ib = identities(a), identities(b)
    def find(g, name):
        return [n for n, (pkg, _, _) in g.items() if pkg.split(" ", 1)[0] == name and pkg.endswith("(path)") and is_library(n)]
    out: list[str] = []
    seen: set[tuple[str, str]] = set()
    def walk(na: str, nb: str) -> None:
        if (na, nb) in seen or ia[na] == ib[nb]:
            return
        seen.add((na, nb))
        pa, fa, ca = a[na]
        _, fb, cb = b[nb]
        if fa != fb:
            out.append(f"{na.rsplit('|', 2)[0]}: features +{sorted(fa - fb)} -{sorted(fb - fa)}")
        def by(cs):
            out: dict[str, list[str]] = {}
            for c in cs:
                out.setdefault(c.rsplit("|", 2)[0], []).append(c)
            return out
        da, db = by(ca), by(cb)
        if set(da) != set(db):
            out.append(f"{na.rsplit('|', 2)[0]}: dependencies +{sorted(set(da) - set(db))} -{sorted(set(db) - set(da))}")
        deeper = len(out)
        for pkg in set(da) & set(db):
            # Without --target a package can be two units alike but for their features (host's,
            # target's): pair each with the one of the same features where there is one.
            for x in da[pkg]:
                same = [y for y in db[pkg] if a[x][1] == b[y][1]]
                walk(x, (same or db[pkg])[0])
        if fa == fb and set(da) == set(db) and len(out) == deeper:
            out.append(f"{pa}: differs with no difference found below it")
    for na in find(a, member):
        for nb in find(b, member):
            walk(na, nb)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--report", action="store_true", help="print every variant, exit 0")
    ap.add_argument("--json", type=Path, help="write the variants here")
    ap.add_argument("--plain", action="store_true", help="select each member alone, without workspace-unify")
    ap.add_argument("--explain", nargs=3, metavar=("MEMBER", "A", "B"),
                    help="where MEMBER's unit selected through A differs from it selected through B "
                         "(`workspace` for the whole workspace)")
    args = ap.parse_args()
    ms = members()
    if args.explain:
        m, a, b = args.explain
        anchor = [] if args.plain else ["-p", workspace_unify.NAME]

        def of(sel: str):
            if sel == "workspace":
                return graph(["--workspace"])
            return graph(["-p", sel, *anchor, *([] if anchor else package_features.arguments([sel]))])
        print(f"== {m} through {a} against {b}")
        ga, gb = of(a), of(b)
        for line in feature_sets(ga, gb, m)[:80]:
            print("  " + line)
        print("== first differences")
        for line in explain(ga, gb, m)[:80]:
            print("  " + line)
        return 0
    names = {n for n, _ in ms} - {workspace_unify.NAME}
    # The whole workspace has workspace-unify, and with it every package's tests' features.
    whole = member_units(graph(["--workspace"]), names)
    variants: dict[str, dict[str, dict]] = {}
    anchor = [] if args.plain else ["-p", workspace_unify.NAME]

    def one(name: str):
        own = [] if anchor else package_features.arguments([name])
        return name, member_units(graph(["-p", name, *anchor, *own]), names)

    with concurrent.futures.ThreadPoolExecutor(8) as pool:
        for sel, units in pool.map(one, sorted(names)):
            for m, us in units.items():
                ref = whole.get(m, {})
                for ident, feats in us.items():
                    if ident in ref:
                        continue
                    wfeats = next(iter(ref.values()), frozenset())
                    v = variants.setdefault(m, {}).setdefault(ident, {"features": sorted(feats), "selections": []})
                    v["selections"].append(sel)
                    v["missing"] = sorted(wfeats - feats)
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(variants, indent=1, sort_keys=True))
    for m in sorted(variants, key=lambda m: -len(variants[m])):
        vs = variants[m]
        print(f"{m}: {len(vs)} unit(s) besides the workspace's")
        for ident, v in sorted(vs.items(), key=lambda kv: -len(kv[1]["selections"])):
            sel = v["selections"]
            own = ("lacks " + ", ".join(v["missing"])) if v["missing"] else "its dependencies differ"
            print(f"  {ident}: {own}; through {len(sel)} selection(s), e.g. {', '.join(sel[:4])}")
    print(f"{len(variants)} of {len(names)} members build differently depending on the selection", file=sys.stderr)
    return 0 if args.report or not variants else 1


if __name__ == "__main__":
    sys.exit(main())
