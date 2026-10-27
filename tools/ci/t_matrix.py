#!/usr/bin/env python3
"""t.toml's checks as t reads them: each `[[check]]` with a `matrix` expanded into the checks it
stands for, as t does before anything else reads the file (t's README, "Matrices").

  [[check]]
  name = "sign-{arch}"
  needs = ["build-{arch}"]
  [check.matrix]
  arch = [{ arch = "x86_64", node = "x86_64" }, { arch = "aarch64", node = "arm64" }]
  app = { from = "apps/*/tilo-app.toml" }       # what `*` stands for in the tree's paths
  exclude = [{ arch = "aarch64", app = "x" }]   # combinations left out
  include = [{ arch = "riscv64", node = "riscv64", app = "y" }]
  [[check.matrix.set]]                          # fields that differ for some combinations
  when = { app = "y" }
  default = false

Each combination becomes a check with `{variable}` replaced by its value in every string and
table key; anything else in braces stays. Checks come in the order `name` uses the axes.

  t_matrix.py [--only PREFIX]   print the expanded checks' names
"""
from __future__ import annotations

import copy
from pathlib import Path
import subprocess
import sys
import tomllib
from typing import Any, Callable

ROOT = Path(__file__).resolve().parents[2]
RESERVED = ("exclude", "include", "set")


class MatrixError(ValueError):
    pass


def from_values(pattern: str, paths: list[str]) -> list[str]:
    """What `*` stands for in the paths matching `pattern`: within one path component, and the
    pattern names a file or a directory (`apps/*` is every entry directly under apps/)."""
    if pattern.count("*") != 1 or any(c in pattern for c in "?[]{}\\") or pattern.startswith("/") \
            or any(part in ("", "..") for part in pattern.split("/")):
        raise MatrixError(f"from = {pattern!r}: name paths below the top of the tree with one `*`")
    prefix, after = pattern.split("*")
    tail, slash, rest = after.partition("/")
    rest = slash + rest
    found = set()
    for path in paths:
        if not path.startswith(prefix):
            continue
        component, slash, beyond = path[len(prefix):].partition("/")
        beyond = slash + beyond
        if not component.endswith(tail) or len(component) == len(tail):
            continue
        if not rest or beyond == rest or beyond.startswith(rest + "/"):
            found.add(component[:len(component) - len(tail)])
    return sorted(found)


def tree_paths(root: Path) -> Callable[[str], list[str]]:
    """The paths of `root` a client sends t: tracked, and untracked but not ignored; every file
    where `root` isn't a Git checkout (a job's sources)."""
    def paths(pattern: str) -> list[str]:
        prefix = pattern.split("*")[0]
        directory = prefix.rpartition("/")[0] + "/" if "/" in prefix else "."
        try:
            listed = subprocess.run(["git", "-C", str(root), "ls-files", "--cached", "--others", "--exclude-standard",
                                     "--", directory], check=True, capture_output=True, text=True).stdout.splitlines()
        except (OSError, subprocess.CalledProcessError):
            listed = [str(p.relative_to(root)) for p in (root / directory).rglob("*") if p.is_file()]
        return from_values(pattern, listed)
    return paths


def replace(text: str, combo: dict[str, str]) -> str:
    out, rest = [], text
    while "{" in rest:
        i = rest.index("{")
        out.append(rest[:i])
        after = rest[i + 1:]
        j = after.find("}")
        if j >= 0 and after[:j] in combo:
            out.append(combo[after[:j]])
            rest = after[j + 1:]
        else:
            out.append("{")
            rest = after
    out.append(rest)
    return "".join(out)


def fill(value: Any, combo: dict[str, str]) -> Any:
    if isinstance(value, str):
        return replace(value, combo)
    if isinstance(value, list):
        return [fill(v, combo) for v in value]
    if isinstance(value, dict):
        return {replace(k, combo): fill(v, combo) for k, v in value.items()}
    return value


def variables(table: Any, known: set[str], where: str) -> dict[str, str]:
    if not isinstance(table, dict):
        raise MatrixError(f"{where}: {table!r} should be a table of variables")
    for k, v in table.items():
        if k not in known:
            raise MatrixError(f"{where}: {k!r} is no variable of the matrix, {sorted(known)}")
        if not isinstance(v, str):
            raise MatrixError(f"{where}: {k} = {v!r} should be a string")
    return dict(table)


def combinations(name: str, matrix: dict[str, Any], paths: Callable[[str], list[str]] | None) -> tuple[list[dict[str, str]], list[tuple[dict[str, str], dict[str, Any]]]]:
    axes = []
    for axis, values in matrix.items():
        if axis in RESERVED:
            continue
        if isinstance(values, dict) and set(values) == {"from"}:
            if paths is None:
                raise MatrixError(f"{name}: axis {axis!r} takes its values from the tree, and there is none")
            values = [{axis: v} for v in paths(values["from"])]
        elif isinstance(values, list):
            values = [{axis: v} if isinstance(v, str) else v for v in values]
            for v in values:
                if not isinstance(v, dict) or axis not in v or not all(isinstance(x, str) for x in v.values()):
                    raise MatrixError(f"{name}: axis {axis!r}: {v!r} should be a string or a table with {axis!r}")
        else:
            raise MatrixError(f"{name}: axis {axis!r} should be a list of values, or {{ from = \"dir/*\" }}")
        keys = set(values[0]) if values else {axis}
        if any(set(v) != keys for v in values):
            raise MatrixError(f"{name}: axis {axis!r}: every value should define the same variables")
        axes.append((axis, keys, values))
    if not axes:
        raise MatrixError(f"{name}: the matrix names no axis")
    known: set[str] = set()
    for _, keys, _ in axes:
        if known & keys:
            raise MatrixError(f"{name}: {sorted(known & keys)} belong to two axes")
        known |= keys

    def position(keys: set[str]) -> int:
        found = [name.find("{" + k + "}") for k in keys if "{" + k + "}" in name]
        return min(found) if found else len(name) + 1
    # Those `name` doesn't use by axis name, as t reads them.
    axes.sort(key=lambda a: (position(a[1]), a[0]))
    combos: list[dict[str, str]] = [{}]
    for _, _, values in axes:
        combos = [{**c, **v} for c in combos for v in values]
    exclude = [variables(e, known, "exclude") for e in matrix.get("exclude", [])]
    combos = [c for c in combos if not any(all(c.get(k) == v for k, v in e.items()) for e in exclude)]
    for extra in matrix.get("include", []):
        extra = variables(extra, known, "include")
        if set(extra) != known:
            raise MatrixError(f"{name}: include: each combination gives every variable, {sorted(known)}")
        if extra not in combos:
            combos.append(extra)
    sets = []
    for entry in matrix.get("set", []):
        entry = dict(entry)
        if "when" not in entry:
            raise MatrixError(f"{name}: set: each entry says `when` it applies")
        when = variables(entry.pop("when"), known, "set.when")
        if {"name", "matrix"} & set(entry):
            raise MatrixError(f"{name}: set can't change `name` or `matrix`")
        sets.append((when, entry))
    return combos, sets


def expand(config: dict[str, Any], paths: Callable[[str], list[str]] | None = None) -> dict[str, Any]:
    """`config` (t.toml as tomllib reads it) with its matrices expanded."""
    out = []
    for check in config.get("check", []):
        if "matrix" not in check:
            out.append(check)
            continue
        template = {k: v for k, v in check.items() if k != "matrix"}
        combos, sets = combinations(check.get("name", ""), check["matrix"], paths)
        for combo in combos:
            one = copy.deepcopy(template)
            for when, fields in sets:
                if all(combo.get(k) == v for k, v in when.items()):
                    one.update(copy.deepcopy(fields))
            out.append(fill(one, combo))
    return {**config, "check": out}


def load(root: Path = ROOT) -> dict[str, Any]:
    """t.toml of `root` with its matrices expanded."""
    return expand(tomllib.loads((root / "t.toml").read_text(encoding="utf-8")), tree_paths(root))


def checks(root: Path = ROOT) -> list[dict[str, Any]]:
    """Every `[[check]]` of `root`'s t.toml as t reads it."""
    return load(root)["check"]


def main(argv: list[str]) -> int:
    only = argv[1] if argv[:1] == ["--only"] and len(argv) > 1 else ""
    for check in checks():
        if check["name"].startswith(only):
            print(check["name"])
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
