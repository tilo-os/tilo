#!/usr/bin/env python3
"""Write the Python SDK reference from the package's own sources.

The public API is `tilo/__init__.py`'s `__all__`: each name is resolved to the
class or function that defines it, and the page shows its signature and its
docstring. A public name, or a public method of one, that has no docstring
stops the generator: the reference is only worth reading while every item on
it says something.

The generated `tilo.types` package is left out: it is written by `tilo sdk
gen` from the apps a workspace runs, so the page says how to make it instead
of listing a snapshot of it.

    python_sdk.py --root <repository> --out <page.md>
"""

from __future__ import annotations

import argparse
import ast
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path


sys.path.insert(0, str(Path(__file__).resolve().parent))
import page  # noqa: E402  (beside this script, not on the isolated path)


PACKAGE = "sdk/python/src/tilo"
PYPROJECT = "sdk/python/pyproject.toml"
TYPES = "sdk/python/src/tilo/types"


# The order a reader meets the SDK in: a name no group holds is an error, so a
# new export cannot reach the page without a place to sit.
GROUPS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("Client", ("Tilo", "AsyncTilo", "SignIn")),
    ("Workspaces", ("Workspaces", "Workspace", "WorkspaceInfo", "WorkspaceList", "Host")),
    ("Resources", ("Resource", "FieldMap", "Listing", "Row", "Hit")),
    ("Runs", ("Run", "RunGroup", "RunOpening", "ProgramAnswer", "Event")),
    ("Files", ("Files",)),
    ("Keys", ("AccountKeys", "WorkspaceKeys", "Key", "MintedKey")),
    ("Members", ("Members", "Member", "Invitation", "Person")),
    ("Usage", ("Usage", "WorkspaceUsage")),
    ("Errors", (
        "Refused", "RateLimited", "AskPending", "ProtocolError",
        "KeyRefused", "KeyRevoked", "KeyExpired",
    )),
)

# Its value is in the lead, where a reader meets the version first.
LEAD_NAMES = frozenset({"__version__"})

# Members a caller never calls: the SDK's own plumbing, named so that dropping
# one is noticed rather than leaving a stale exemption behind.
INTERNAL_MEMBERS = frozenset({
    ("AsyncTilo", "handoff"),
    ("RunGroup", "note"),
})

# A classmethod that reads the value out of the door's answer. Every model has
# one under the same name, and the SDK calls it.
INTERNAL_METHOD = "of"


@dataclass(frozen=True)
class Item:
    """One public name and where the SDK defines it."""

    name: str
    kind: str  # "class" or "function"
    node: ast.ClassDef | ast.FunctionDef | ast.AsyncFunctionDef
    source: str


def generate(arguments: argparse.Namespace) -> str:
    root = arguments.root.resolve()
    package = root / PACKAGE
    if not package.is_dir():
        raise page.ReferenceError(f"{PACKAGE} is not there: {package}")
    if not (root / TYPES).is_dir():
        raise page.ReferenceError(f"{TYPES} is not there: run `tilo sdk gen` writes it")
    exported, version = module_exports(package / "__init__.py")
    defined = definitions(package, root)
    items = resolve(exported, defined)
    project = project_facts(root / PYPROJECT)
    if version != project["version"]:
        raise page.ReferenceError(
            f"{PACKAGE}/__init__.py says {version} and {PYPROJECT} says {project['version']}"
        )
    placed = [name for _, names in GROUPS for name in names]
    unknown = sorted(set(placed) - set(exported))
    unplaced = sorted(set(exported) - set(placed) - LEAD_NAMES)
    if unknown:
        raise page.ReferenceError(f"groups name what is not exported: {', '.join(unknown)}")
    if unplaced:
        raise page.ReferenceError(f"no group holds: {', '.join(unplaced)}")
    repeated = {name for name in placed if placed.count(name) > 1}
    if repeated:
        raise page.ReferenceError(f"a name is in two groups: {', '.join(sorted(repeated))}")
    check_internal(items)
    undocumented(items)

    lead = package_docstring(package / "__init__.py")
    sections = [
        page.prose(
            f"{project['summary']} This page is generated from the package's own "
            f"sources and documents {project['name']} {version}; the section on app "
            f"types says how to add the apps a workspace runs."
        ),
        page.code("sh", f"pip install {project['name']}"),
    ]
    for title, names in GROUPS:
        sections.append(page.heading(2, title))
        sections.extend(render(items[name]) for name in names)
    sections.extend(app_types())
    return page.page("Python SDK", lead, sections)


def module_exports(source: Path) -> tuple[list[str], str]:
    """`__all__` in the order it is written, and `__version__`."""
    if not source.is_file():
        raise page.ReferenceError(f"the package has no {source.name}: {source}")
    module = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
    exported: list[str] | None = None
    version: str | None = None
    for node in module.body:
        if isinstance(node, ast.Assign):
            for target in node.targets:
                if not isinstance(target, ast.Name):
                    continue
                if target.id == "__all__":
                    exported = string_list(node.value, source)
                elif target.id == "__version__" and isinstance(node.value, ast.Constant):
                    version = str(node.value.value)
    if exported is None:
        raise page.ReferenceError(f"{source}: no __all__ names the public API")
    if version is None:
        raise page.ReferenceError(f"{source}: no __version__")
    return exported, version


def string_list(value: ast.AST, source: Path) -> list[str]:
    if not isinstance(value, ast.List) or not all(
        isinstance(item, ast.Constant) and isinstance(item.value, str) for item in value.elts
    ):
        raise page.ReferenceError(f"{source}: __all__ is a list of names, written out")
    return [str(item.value) for item in value.elts]  # type: ignore[attr-defined]


def definitions(package: Path, root: Path) -> dict[str, list[Item]]:
    """Every top-level class and function, by the name it is defined under."""
    found: dict[str, list[Item]] = {}
    for source in sorted(package.glob("*.py")):
        module = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
        for node in module.body:
            if isinstance(node, ast.ClassDef):
                kind = "class"
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                kind = "function"
            else:
                continue
            item = Item(node.name, kind, node, source.relative_to(root).as_posix())
            found.setdefault(node.name, []).append(item)
    return found


def resolve(exported: list[str], defined: dict[str, list[Item]]) -> dict[str, Item]:
    items: dict[str, Item] = {}
    for name in exported:
        if name in LEAD_NAMES:
            continue
        candidates = defined.get(name, [])
        if not candidates:
            raise page.ReferenceError(f"{name} is exported but no module defines it")
        if len(candidates) > 1:
            where = ", ".join(item.source for item in candidates)
            raise page.ReferenceError(f"{name} is defined in more than one place: {where}")
        items[name] = candidates[0]
    return items


def project_facts(pyproject: Path) -> dict[str, str]:
    try:
        data = tomllib.loads(pyproject.read_text(encoding="utf-8"))
    except OSError as error:
        raise page.ReferenceError(f"{pyproject}: {error}") from error
    project = data.get("project") or {}
    for key in ("name", "version", "description"):
        if not project.get(key):
            raise page.ReferenceError(f"{pyproject}: project.{key} is missing")
    return {
        "name": str(project["name"]),
        "version": str(project["version"]),
        "summary": str(project["description"]),
    }


def package_docstring(source: Path) -> str:
    module = ast.parse(source.read_text(encoding="utf-8"), filename=str(source))
    text = ast.get_docstring(module)
    if not text:
        raise page.ReferenceError(f"{source}: the package needs a docstring to open the page")
    return text


def check_internal(items: dict[str, Item]) -> None:
    """An exemption that names nothing is a stale exemption."""
    for owner, name in sorted(INTERNAL_MEMBERS):
        item = items.get(owner)
        names = {
            child.name
            for child in (item.node.body if item is not None else [])
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
        }
        if name not in names:
            raise page.ReferenceError(f"{owner}.{name} is exempt from docs but no longer exists")


def undocumented(items: dict[str, Item]) -> None:
    """Every public name and public method says something, or the page stops here."""
    missing: list[str] = []
    for name in sorted(items):
        item = items[name]
        if not ast.get_docstring(item.node):
            missing.append(f"{name} ({item.source})")
            continue
        for member in public_members(item.node):
            if not ast.get_docstring(member):
                missing.append(f"{item.name}.{member.name} ({item.source})")
    if missing:
        raise page.ReferenceError(
            "public items with no docstring:\n  " + "\n  ".join(missing)
        )


def public_members(node: ast.AST) -> list[ast.FunctionDef | ast.AsyncFunctionDef]:
    """The methods and properties a caller meets, in the order they are written."""
    if not isinstance(node, ast.ClassDef):
        return []
    return [
        child
        for child in node.body
        if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef))
        and not child.name.startswith("_")
        and not is_internal(node.name, child)
    ]


def is_internal(owner: str, node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    if (owner, node.name) in INTERNAL_MEMBERS:
        return True
    return node.name == INTERNAL_METHOD and any(
        isinstance(decorator, ast.Name) and decorator.id == "classmethod"
        for decorator in node.decorator_list
    )


def render(item: Item) -> str:
    doc = ast.get_docstring(item.node)
    if not doc:
        raise page.ReferenceError(f"{item.source}: {item.name} is exported and has no docstring")
    name = item.name
    if item.kind == "function":
        return "\n\n".join([
            page.heading(3, name),
            page.code("python", signature(item.node, None, self_=False)),
            page.prose(doc),
        ])
    assert isinstance(item.node, ast.ClassDef)
    parts = [
        page.heading(3, name),
        page.code("python", class_signature(item.node, name)),
    ]
    parts.append(page.prose(doc))
    constructor = next(
        (child for child in item.node.body
         if isinstance(child, ast.FunctionDef) and child.name == "__init__"),
        None,
    )
    if constructor is not None and ast.get_docstring(constructor):
        parts.append(page.prose(ast.get_docstring(constructor) or ""))
    for member in public_members(item.node):
        parts.extend(render_member(name, member))
    return "\n\n".join(parts)


def render_member(owner: str, node: ast.FunctionDef | ast.AsyncFunctionDef) -> list[str]:
    doc = ast.get_docstring(node)
    if not doc:
        raise page.ReferenceError(
            f"{owner}.{node.name} is public with no docstring: document it, or name it internal"
        )
    parts = [page.heading(4, f"{owner}.{node.name}")]
    if is_property(node):
        returns = annotation(node.returns)
        parts.append(page.code("python", f"{owner}.{node.name}: {returns or 'Any'}"))
    else:
        parts.append(page.code("python", signature(node, f"{owner}.{node.name}")))
    parts.append(page.prose(doc))
    return parts


def is_property(node: ast.FunctionDef | ast.AsyncFunctionDef) -> bool:
    return any(
        isinstance(decorator, ast.Name) and decorator.id == "property"
        for decorator in node.decorator_list
    )


def class_signature(node: ast.ClassDef, name: str) -> str:
    bases = ", ".join(ast.unparse(base) for base in node.bases)
    lines = [f"class {name}({bases})" if bases else f"class {name}"]
    constructor = next(
        (child for child in node.body
         if isinstance(child, ast.FunctionDef) and child.name == "__init__"),
        None,
    )
    if constructor is not None:
        lines.append(signature(constructor, name))
    elif fields(node):
        lines.append(f"{name}({fields(node)})")
    return "\n".join(lines)


def fields(node: ast.ClassDef) -> str:
    """A dataclass's own fields as its constructor takes them."""
    if not any(
        (isinstance(decorator, ast.Name) and decorator.id == "dataclass")
        or (isinstance(decorator, ast.Call) and getattr(decorator.func, "id", "") == "dataclass")
        for decorator in node.decorator_list
    ):
        return ""
    written: list[str] = []
    for child in node.body:
        if not isinstance(child, ast.AnnAssign) or not isinstance(child.target, ast.Name):
            continue
        name = child.target.id
        if name.startswith("_"):
            continue
        entry = f"{name}: {annotation(child.annotation) or 'Any'}"
        if child.value is not None:
            entry = f"{entry} = {ast.unparse(child.value)}"
        written.append(entry)
    return ", ".join(written)


def signature(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    written_as: str | None = None,
    self_: bool = True,
) -> str:
    """`written_as(parameters) -> returns`, with `self` or `cls` left out.

    `written_as` is how a caller writes the call: `Workspace.ls`, or `Tilo` for
    a constructor. Without one it is the function's own name.
    """
    arguments = node.args
    positional = [*arguments.posonlyargs, *arguments.args]
    defaults: list[ast.AST | None] = [None] * (len(positional) - len(arguments.defaults))
    defaults.extend(arguments.defaults)
    written: list[str] = []
    for index, (argument, default) in enumerate(zip(positional, defaults)):
        if index == 0 and self_:
            continue
        written.append(parameter(argument, default))
    if arguments.vararg is not None:
        written.append("*" + parameter(arguments.vararg, None))
    elif arguments.kwonlyargs:
        written.append("*")
    for argument, default in zip(arguments.kwonlyargs, arguments.kw_defaults):
        written.append(parameter(argument, default))
    if arguments.kwarg is not None:
        written.append("**" + parameter(arguments.kwarg, None))
    prefix = f"{written_as or node.name}("
    line = prefix + ", ".join(written) + ")"
    returns = annotation(node.returns) if node.returns is not None else ""
    return f"{line} -> {returns}" if returns and returns != "None" else line


def parameter(argument: ast.arg, default: ast.AST | None) -> str:
    text = argument.arg
    if argument.annotation is not None:
        text = f"{text}: {annotation(argument.annotation)}"
    if default is not None:
        text = f"{text} = {ast.unparse(default)}"
    return text


def annotation(node: ast.AST | None) -> str:
    return ast.unparse(node) if node is not None else ""


def app_types() -> list[str]:
    return [
        page.heading(2, "App types"),
        page.prose(
            "Every app a workspace runs publishes its resources and actions, and the "
            "SDK can write them as typed Python classes so an editor completes a "
            "resource's fields and its actions' arguments. The types follow the apps "
            "the workspace runs, so they are written from it rather than shipped with "
            "the package: `tilo sdk gen` reads a workspace and writes the "
            "`tilo.types` package."
        ),
        page.code("sh", "tilo sdk gen --lang python --out tilo/types"),
        page.prose(
            "Import a type from `tilo.types` and read a resource with it; the "
            "workspace answers the fields the app publishes now. Run the command "
            "again after an app updates, so the types name what it offers."
        ),
    ]


if __name__ == "__main__":
    sys.exit(page.main(generate, __doc__ or "Write the Python SDK reference."))
