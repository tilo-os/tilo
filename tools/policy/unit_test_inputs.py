#!/usr/bin/env python3
"""What a test file of tools/tests reads, so its check reruns only when that changes.

  unit_test_inputs.py show FILE...   the inputs of each file, or why it reads the whole tree

A test reads its own source, the modules it imports, the files and directories it names (string
literals and `ROOT / "a" / "b"` chains that are paths of the tree, and the globs it runs over
them), and, transitively, what the scripts and modules among those read. A named directory counts
as everything below it.

Some read the tree as a whole: a script among them lists it through Git (`git ls-files`, `git
grep`), or walks the top of the tree itself (`os.walk(ROOT)`, `find "$repo"`). Those keep the
whole tree as their input (`inputs` returns None with the reason).

What a test builds at run time (a path joined from variables) is invisible here, so the inputs
are enforced, not trusted: tools/policy/run_unit_tests.py --pruned runs a test in a copy of the
tree holding only its inputs, where a read of anything else finds nothing and fails the test.
"""
from __future__ import annotations

import ast
import bisect
import copy
from functools import lru_cache
import os
import posixpath
from pathlib import Path, PurePosixPath
import re
import shutil
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402

# Never part of the tree a test sees: Git's own data and build output.
SKIP = frozenset({".git", "target", "node_modules", "__pycache__"})
SCRIPT = (".py", ".sh", ".bash", ".mjs", ".js")
# A quoted path-looking string in a script of any language.
QUOTED = re.compile(r"""["']\.?/?([A-Za-z0-9_.@+-]+(?:/[A-Za-z0-9_.@+*?{}\[\]-]+)+/?)["']""")
# An unquoted path-looking word in a shell script or a command line.
WORD = re.compile(r"(?<![\w$./-])\.?/?([A-Za-z0-9_.@+-]+(?:/[A-Za-z0-9_.@+-]+)+)")
# A sibling a script names by file name alone (`"$(dirname "$0")/lib.sh"`, `here / "fixture.json"`).
SIBLING = re.compile(r"(?<![\w.-])([\w.-]+\.(?:sh|bash|py|mjs|js|json|toml|txt|yml|yaml|conf|service))\b")
# Names that hold the top of the tree in Python scripts.
ROOT_NAMES = frozenset({"ROOT", "REPO", "REPO_ROOT", "REPOSITORY", "TOP"})
# Names a helper's functions often take the top of the tree as (`def all_apps(repo)`): what they
# join onto it is taken as a path of the tree, since a caller may pass the real one. A test's own
# `root` is usually a scratch directory, so only a helper's walk of one reads the whole tree.
SOFT_ROOT_NAMES = frozenset({"repo", "root", "repository", "repo_root", "top"})
# A shell variable set to the top of the tree, from the script's own location or Git.
SHELL_ROOT = re.compile(
    r"""^\s*(?:readonly\s+|local\s+|export\s+)?([A-Za-z_]\w*)=["']?\$\(\s*(?:cd\s+"?\$\(dirname[^\n]*\.\./\.\.|git\s+rev-parse\s+--show-toplevel)""",
    re.MULTILINE)
GIT_LISTING = re.compile(r"""\bgit\b[^\n]{0,80}\b(?:ls-files|grep)\b|["']ls-files["']|["']git["']\s*,\s*["']grep["']""")


class Tree:
    """The files and directories of a tree, as a test would see them."""

    def __init__(self, root: Path):
        self.root = root
        self.files: set[str] = set()
        self.dirs: set[str] = set()
        for top, directories, names in os.walk(root):
            directories[:] = sorted(d for d in directories if d not in SKIP)
            relative = Path(top).relative_to(root).as_posix()
            if relative != ".":
                self.dirs.add(relative)
            for name in names:
                if name not in SKIP:  # a worktree's `.git` is a file
                    self.files.add(name if relative == "." else f"{relative}/{name}")

    def resolve(self, word: str, here: str) -> str | None:
        """`word` as a path of the tree (a file, or `dir/**`), from the top or from `here`."""
        word = word.strip().strip("/")
        if word.startswith("./"):
            word = word[2:]
        if not word or word.startswith("../") or "${" in word or "$(" in word:
            return None
        for base in ("", here):
            path = f"{base}/{word}" if base else word
            path = os.path.normpath(path) if ".." in path else path
            if any(c in path for c in "*?["):
                return path if self._any(path) else None
            if path in self.files:
                return path
            if path in self.dirs:
                return f"{path}/**"
        return None

    @lru_cache(maxsize=None)
    def _any(self, pattern: str) -> bool:
        return next(self.matching(pattern), None) is not None

    def matching(self, pattern: str):
        """The files `pattern` matches, looking only at those under its literal prefix."""
        if not hasattr(self, "_sorted"):
            self._sorted = sorted(self.files)
        prefix = re.split(r"[*?\[]", pattern, maxsplit=1)[0]
        matcher = compile_pattern(pattern)
        for path in self._sorted[bisect.bisect_left(self._sorted, prefix):]:
            if not path.startswith(prefix):
                break
            if matcher.match(path):
                yield path


@lru_cache(maxsize=None)
def compile_pattern(pattern: str) -> re.Pattern[str]:
    """t's input globs: `**` any number of directories, `*` and `?` within one."""
    out, i = [], 0
    while i < len(pattern):
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?")
            i += 3
        elif pattern.startswith("**", i):
            out.append(".*")
            i += 2
        elif pattern[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pattern[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pattern[i]))
            i += 1
    return re.compile("".join(out) + r"\Z")


def matches(path: str, patterns: list[str]) -> bool:
    return any(compile_pattern(p).match(path) for p in patterns)


def _part(node: ast.AST) -> str | None:
    """A literal path part: a string, or an f-string whose substitutions become `*`."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.JoinedStr):
        return "".join(v.value if isinstance(v, ast.Constant) else "*" for v in node.values)
    return None


def _up(path: str | None, levels: int) -> str | None:
    """`path`'s directory `levels` up, None above the top of the tree."""
    for _ in range(levels):
        if path is None or path == "":
            return None
        path = posixpath.dirname(path)
    return path


def _chain(node: ast.AST, names: dict[str, str], here: str) -> str | None:
    """A path expression of the tree as a path from its top (`""` for the top itself): names of
    the top, `Path(__file__)` (`here`) and its parents, `/` with literal parts, names bound to such
    an expression. None for anything else."""
    if isinstance(node, ast.Name):
        if node.id in ROOT_NAMES:
            return ""
        if node.id == "__file__":
            return here
        if node.id in names:
            return names[node.id]
        return "" if node.id in SOFT_ROOT_NAMES else None
    if isinstance(node, ast.Call):
        func = node.func
        if isinstance(func, ast.Name) and func.id in ("Path", "PurePath", "PosixPath", "str") and len(node.args) == 1:
            inner = node.args[0]
            if isinstance(inner, ast.Name) and inner.id == "__file__":
                return here
            return _chain(inner, names, here)
        if isinstance(func, ast.Attribute) and func.attr in ("resolve", "absolute"):
            return _chain(func.value, names, here)
        if isinstance(func, ast.Attribute) and func.attr == "with_name" and node.args:
            base, part = _chain(func.value, names, here), _part(node.args[0])
            return None if base is None or part is None else posixpath.join(posixpath.dirname(base), part)
        if isinstance(func, ast.Attribute) and func.attr == "dirname" and node.args:  # os.path.dirname
            return _up(_chain(node.args[0], names, here), 1)
        if isinstance(func, ast.Attribute) and func.attr == "join" and node.args:  # os.path.join
            base = _chain(node.args[0], names, here)
            parts = [_part(a) for a in node.args[1:]]
            return None if base is None or None in parts else posixpath.join(base, *parts).strip("/")
        return None
    if isinstance(node, ast.Attribute) and node.attr == "parent":
        return _up(_chain(node.value, names, here), 1)
    if (isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute) and node.value.attr == "parents"
            and isinstance(node.slice, ast.Constant) and isinstance(node.slice.value, int)):
        return _up(_chain(node.value.value, names, here), node.slice.value + 1)
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
        left, part = _chain(node.left, names, here), _part(node.right)
        if left is None or part is None:
            return None
        joined = posixpath.normpath(posixpath.join(left, part)) if left or part else ""
        return None if joined.startswith("..") else ("" if joined == "." else joined)
    return None


def _expand(node: ast.AST, loops: dict[str, list[str]], names: dict[str, str], here: str) -> set[str]:
    """A path expression whose f-string parts substitute loop variables over literal values, as
    each path it takes; empty when something else is substituted."""
    holes = [v.value for v in ast.walk(node) if isinstance(v, ast.FormattedValue)]
    if not holes or not all(isinstance(h, ast.Name) and h.id in loops for h in holes):
        return set()
    variables = sorted({h.id for h in holes})
    out: set[str] = set()

    def bind(index: int, values: dict[str, str]) -> None:
        if index == len(variables):
            class Fill(ast.NodeTransformer):
                def visit_JoinedStr(self, joined):
                    return ast.Constant("".join(v.value if isinstance(v, ast.Constant) else values[v.value.id]
                                                for v in joined.values))
            filled = _chain(Fill().visit(copy.deepcopy(node)), names, here)
            if filled:
                out.add(filled)
            return
        for value in loops[variables[index]]:
            bind(index + 1, {**values, variables[index]: value})

    bind(0, {})
    return out


def _python_reads(text: str, here: str) -> tuple[set[str], set[str], bool, set[str], bool, set[str]]:
    """A Python file's path words, the globs it runs over the tree, whether it walks the top of
    the tree in a way no glob describes, the modules it loads by path (patterns), and whether
    it runs `git ls-files` or `git grep`."""
    words: set[str] = set()
    globs: set[str] = set()
    loads: set[str] = set()
    modules: set[str] = set()
    search: list[str] = []
    walks_top = lists = False
    try:
        tree = ast.parse(text)
    except SyntaxError:
        return words, globs, walks_top, loads, lists, set()
    names: dict[str, str] = {}
    loops: dict[str, list[str]] = {}
    docstrings: set[int] = set()
    for node in ast.walk(tree):
        # `def check(root: Path = ROOT)`: the parameter holds what its default does.
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            arguments = node.args.posonlyargs + node.args.args
            for argument, default in zip(arguments[len(arguments) - len(node.args.defaults):], node.args.defaults):
                chain = _chain(default, names, here)
                if chain is not None:
                    names[argument.arg] = chain
            for argument, default in zip(node.args.kwonlyargs, node.args.kw_defaults):
                chain = _chain(default, names, here) if default is not None else None
                if chain is not None:
                    names[argument.arg] = chain
        if (isinstance(node, ast.For) and isinstance(node.target, ast.Name)
                and isinstance(node.iter, (ast.Tuple, ast.List))
                and all(isinstance(e, ast.Constant) and isinstance(e.value, str) for e in node.iter.elts)):
            loops[node.target.id] = [e.value for e in node.iter.elts]
        if isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant):
            docstrings.add(id(node.value))
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            chain = _chain(node.value, names, here)
            if chain is not None:
                names[node.targets[0].id] = chain
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str) and id(node) not in docstrings:
            words.update(node.value.split())
            if GIT_LISTING.match(node.value.strip()):
                lists = True
        if isinstance(node, (ast.List, ast.Tuple)):
            items = [e.value for e in node.elts if isinstance(e, ast.Constant) and isinstance(e.value, str)]
            if "git" in items and ({"ls-files", "grep"} & set(items[items.index("git"):])):
                lists = True
        chain = _chain(node, names, here) if isinstance(node, (ast.BinOp, ast.Call)) else None
        if chain:
            words.add(chain)
        if isinstance(node, ast.Import):
            modules.update(alias.name for alias in node.names)
        if isinstance(node, ast.ImportFrom) and node.module and not node.level:
            modules.add(node.module)
            modules.update(f"{node.module}.{alias.name}" for alias in node.names)
        if isinstance(node, ast.ImportFrom) and node.level == 1:
            modules.update(alias.name for alias in node.names)
        if isinstance(node, ast.Call):
            func = node.func
            # `sys.path.insert(0, str(ROOT / "tools/apps"))`: where its imports are found.
            if (isinstance(func, ast.Attribute) and func.attr in ("insert", "append") and node.args
                    and ast.unparse(func.value) == "sys.path"):
                directory = _chain(node.args[-1], names, here)
                if directory is not None:
                    search.append(directory)
            loader = func.attr if isinstance(func, ast.Attribute) else func.id if isinstance(func, ast.Name) else ""
            if loader in ("spec_from_file_location", "run_path", "SourceFileLoader") and node.args:
                argument = node.args[-1] if loader != "run_path" else node.args[0]
                loaded = _chain(argument, names, here)
                if loaded and "*" in loaded:
                    # `for name in ("a", "b"): ... f"policy/{name}.py"`: the names it loops over.
                    loaded = _expand(argument, loops, names, here) or {loaded}
                    loads.update(loaded)
                elif loaded:
                    loads.add(loaded)
            if isinstance(func, ast.Attribute) and func.attr in ("glob", "rglob", "iterdir"):
                base = _chain(func.value, names, here)
                if base is None:
                    continue
                pattern = node.args[0].value if node.args and isinstance(node.args[0], ast.Constant) else "*"
                if func.attr == "rglob":
                    pattern = f"**/{pattern}"
                soft = isinstance(func.value, ast.Name) and func.value.id in SOFT_ROOT_NAMES
                if base == "" and soft and PurePosixPath(here).name.startswith("test_"):
                    continue  # a test's own scratch `root`
                if not isinstance(pattern, str) or (base == "" and any(c in pattern.split("/")[0] for c in "*?[")):
                    # A glob from the top whose first part is a pattern (`ROOT.rglob("*")`) lists it all.
                    walks_top = True
                    continue
                globs.add(f"{base}/{pattern}".strip("/") if base else pattern)
            elif isinstance(func, ast.Attribute) and func.attr in ("walk", "listdir", "scandir") and node.args:
                base = _chain(node.args[0], names, here)
                if base is None and isinstance(node.args[0], ast.Call) and node.args[0].args:
                    base = _chain(node.args[0].args[0], names, here)  # os.walk(str(ROOT / "x"))
                if base is not None:
                    soft = isinstance(node.args[0], ast.Name) and node.args[0].id in SOFT_ROOT_NAMES
                    if base:
                        globs.add(f"{base}/**")
                    elif not (soft and PurePosixPath(here).name.startswith("test_")):
                        walks_top = True
    # Its imports, as the repository's modules they are: beside it, in the directories it puts on
    # sys.path, the policy suite's, or under tools/ by their dotted name.
    imported: set[str] = set()
    for module in modules:
        parts = module.split(".")
        for directory in (posixpath.dirname(here), *search, *steps.MODULE_DIRS, "tools", ""):
            for candidate in (posixpath.join(directory, *parts) + ".py", posixpath.join(directory, *parts, "__init__.py"),
                              posixpath.join(directory, parts[-1]) + ".py"):
                imported.add(candidate.strip("/"))
    return words, globs, walks_top, loads, lists, imported


def _reads_of(path: str, tree: Tree) -> tuple[set[str], str | None, set[str]]:
    key = (id(tree), path)
    if key not in _READS:
        _READS[key] = _read_file(path, tree)
    return _READS[key]


_READS: dict[tuple[int, str], tuple[set[str], str | None, set[str]]] = {}


def _read_file(path: str, tree: Tree) -> tuple[set[str], str | None, set[str]]:
    """What one file names (paths of the tree), why it reads the whole tree if it does, and the
    patterns of the modules it loads by path."""
    loads: set[str] = set()
    try:
        text = (tree.root / path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return set(), None, loads
    here = str(PurePosixPath(path).parent)
    found: set[str] = set()
    whole = None
    if path.endswith(".py"):
        words, globs, walks_top, loads, lists, imported = _python_reads(text, path)
        if lists:
            whole = f"{path} lists the tree through Git"
        if walks_top:
            whole = whole or f"{path} walks the top of the tree"
        for pattern in globs:
            if any(c in pattern for c in "*?[") and tree._any(pattern):
                found.add(pattern)
            # What a glob or listing returns may be a directory, which the script may then read;
            # a glob across directories (`apps/*/tilo-app.toml`) finds them to read what is beside.
            if tree._any(pattern.rstrip("*").rstrip("/") + "/**" if pattern.endswith("/**") else pattern + "/**"):
                found.add(pattern if pattern.endswith("/**") else pattern + "/**")
            directory = posixpath.dirname(pattern)
            if (any(c in directory for c in "*?[") and not directory.endswith("**")
                    and not any(c in directory.split("/")[0] for c in "*?[")):
                found.add(f"{directory}/**")
            elif (resolved := tree.resolve(pattern, "")) is not None:
                found.add(resolved)
        found |= {candidate for candidate in imported if candidate in tree.files and candidate != path}
    else:
        # What a shell script says in its comments is not what it runs.
        text = "\n".join(re.sub(r"(?:^|\s)#.*$", "", line) for line in text.splitlines())
        if GIT_LISTING.search(text):
            whole = f"{path} lists the tree through Git"
        words = {m[1] for m in QUOTED.finditer(text)} | {m[1] for m in WORD.finditer(text)}
        for name in SHELL_ROOT.findall(text):
            if re.search(rf"""\bfind\s+["']?\${{?{name}\b(?![^\s"']*/)""", text):
                whole = whole or f"{path} walks the top of the tree"
    words |= {m[1] for m in SIBLING.finditer(text if not path.endswith(".py") else " ".join(words))}
    for word in words:
        resolved = tree.resolve(word, here)
        # A bare word names a file beside the script or at the top (`Cargo.toml`), never a
        # directory: "os" or "apps" in a message is not a read of that directory.
        if resolved is not None and resolved != "**" and ("/" in word.strip("/") or resolved in tree.files):
            found.add(resolved)
    found |= {p for p in loads if tree._any(p)}
    return found, whole, loads


def inputs(test: str, tree: Tree) -> tuple[list[str] | None, str]:
    """The inputs of `test` (a path of the tree), or None and why it reads the whole tree."""
    seen, frontier, reads = {test}, [test], {test}
    while frontier:
        path = frontier.pop()
        found, whole, loads = _reads_of(path, tree)
        if whole:
            return None, whole
        # The modules it loads by a pattern (`TOOLS / f"policy/{name}.py"`) are each followed.
        loaded = {script for pattern in loads for script in tree.matching(pattern)}
        for read in [*found, *sorted(loaded)]:
            if read in reads and read not in loaded:
                continue
            reads.add(read)
            # A script it names reads what it reads.
            for script in [read] if read in tree.files else []:
                if script.endswith(SCRIPT) and script not in seen:
                    seen.add(script)
                    frontier.append(script)
    # A directory below another is covered by it.
    dirs = sorted(r[:-3] for r in reads if r.endswith("/**"))
    covered = [d for d in dirs if any(d != o and d.startswith(o + "/") for o in dirs)]
    kept = sorted(r for r in reads if not (r.endswith("/**") and r[:-3] in covered)
                  and not any(r.startswith(d + "/") and not r.endswith("/**") for d in dirs))
    return kept, ""


# Files above this size are linked into a pruned tree rather than copied: large data (fonts,
# images), whose own location nothing resolves. The rest is copied, so that a script finding the
# top of the tree from its own location, or a path resolved against it, stays in the pruned one.
COPY_BELOW = 256 * 1024


def prune(tree: Tree, patterns: list[str], destination: Path) -> int:
    """Make `destination` a tree of just the files `patterns` match. The number of files."""
    count = 0
    for path in sorted(tree.files):
        if not matches(path, patterns):
            continue
        source, target = tree.root / path, destination / path
        target.parent.mkdir(parents=True, exist_ok=True)
        if source.is_symlink():
            target.symlink_to(os.readlink(source))
        elif source.stat().st_size >= COPY_BELOW:
            target.symlink_to(source.resolve())
        else:
            shutil.copy2(source, target)
        count += 1
    return count


def main(argv: list[str]) -> int:
    if argv[:1] != ["show"] or len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    tree = Tree(ROOT)
    for test in argv[1:]:
        found, why = inputs(test, tree)
        print(f"{test}: {why}" if found is None else f"{test}:\n  " + "\n  ".join(found))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
