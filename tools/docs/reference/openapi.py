#!/usr/bin/env python3
"""Write a REST reference from the server's OpenAPI description.

One section per tag, one `###` per operation, with the method and path, what
the call does, its parameters, the shape of what it takes and what comes back,
and a `curl` line that runs it. The description is the server's own: an
operation or parameter with nothing said about it stops the generator, so the
reference cannot quietly list a call nobody explained.

    openapi.py --root <repository> --out <page.md> [--spec <openapi.json>]

Without `--spec`, the one `openapi*.json` under the repository is read; a tree
with none, or with several, names them and asks for the flag.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parent))
import page  # noqa: E402  (beside this script, not on the isolated path)


METHODS = ("get", "put", "post", "delete", "options", "head", "patch", "trace")
KEY = "TILO_API_KEY"


@dataclass(frozen=True)
class Operation:
    method: str
    path: str
    tag: str
    written: dict[str, Any]


def generate(arguments: argparse.Namespace) -> str:
    spec = description(arguments.root.resolve(), arguments.spec)
    title = (spec.get("info") or {}).get("title") or ""
    if not title:
        raise page.ReferenceError("the description has no info.title to name the page")
    lead = (spec.get("info") or {}).get("summary") or (spec.get("info") or {}).get("description") or ""
    if not lead.strip():
        raise page.ReferenceError("the description says nothing about the API (info.summary)")
    base = server(spec)
    operations = collect(spec)
    if not operations:
        raise page.ReferenceError("the description has no operations")
    sections: list[str] = []
    if bearer(spec):
        sections.append(page.prose(
            "Every call carries a key as `Authorization: Bearer $TILO_API_KEY`. A key "
            "minted for one workspace reaches that workspace and nothing else; the "
            "commands of the command line that mint one are under `tilo keys`."
        ))
    documented = {tag["name"]: tag for tag in spec.get("tags") or [] if isinstance(tag, dict)}
    for tag in order(spec, operations):
        sections.append(page.heading(2, tag))
        said = documented.get(tag, {}).get("description")
        if said:
            sections.append(page.prose(said))
        for operation in operations:
            if operation.tag == tag:
                sections.extend(render(operation, spec, base))
    return page.page(title, lead, sections)


def description(root: Path, named: Path | None) -> dict[str, Any]:
    path = named if named is not None else find(root)
    try:
        spec = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise page.ReferenceError(f"{path}: {error}") from error
    if not isinstance(spec, dict):
        raise page.ReferenceError(f"{path}: an OpenAPI description is a JSON object")
    version = str(spec.get("openapi") or "")
    if not version.startswith("3."):
        raise page.ReferenceError(f"{path}: openapi says {version!r}, not 3.x")
    return spec


def find(root: Path) -> Path:
    """The one OpenAPI description in the tree."""
    skipped = {".git", ".claude", "node_modules", "target", "vendor"}
    found = sorted(
        path for path in root.rglob("openapi*.json")
        if not any(part in skipped for part in path.relative_to(root).parts)
    )
    if not found:
        raise page.ReferenceError(
            "no openapi*.json under the repository: name the description with --spec"
        )
    if len(found) > 1:
        listed = ", ".join(path.relative_to(root).as_posix() for path in found)
        raise page.ReferenceError(f"more than one OpenAPI description ({listed}): name one with --spec")
    return found[0]


def bearer(spec: dict[str, Any]) -> bool:
    """Whether the description says a key travels in an Authorization header."""
    schemes = ((spec.get("components") or {}).get("securitySchemes") or {}).values()
    return any(
        isinstance(scheme, dict)
        and str(scheme.get("type") or "") == "http"
        and str(scheme.get("scheme") or "").lower() == "bearer"
        for scheme in schemes
    )


def server(spec: dict[str, Any]) -> str:
    servers = spec.get("servers") or []
    if servers and isinstance(servers[0], dict) and servers[0].get("url"):
        return str(servers[0]["url"]).rstrip("/")
    return "https://tilo.sh"


def collect(spec: dict[str, Any]) -> list[Operation]:
    operations: list[Operation] = []
    paths = spec.get("paths") or {}
    if not isinstance(paths, dict):
        raise page.ReferenceError("paths is not an object")
    for path in sorted(paths):
        item = paths[path]
        if not isinstance(item, dict):
            continue
        for method in METHODS:
            written = item.get(method)
            if not isinstance(written, dict):
                continue
            tags = written.get("tags") or []
            tag = str(tags[0]) if tags else "Other"
            operations.append(Operation(method, path, tag, written))
    return operations


def order(spec: dict[str, Any], operations: list[Operation]) -> list[str]:
    """The tags: the description's own order first, then as the paths meet them."""
    declared = [
        str(tag["name"])
        for tag in spec.get("tags") or []
        if isinstance(tag, dict) and tag.get("name")
    ]
    seen = [tag for tag in declared if any(operation.tag == tag for operation in operations)]
    for operation in operations:
        if operation.tag not in seen:
            seen.append(operation.tag)
    return seen


def render(operation: Operation, spec: dict[str, Any], base: str) -> list[str]:
    written = operation.written
    said = (written.get("summary") or "").strip()
    detail = (written.get("description") or "").strip()
    if not said and not detail:
        raise page.ReferenceError(
            f"{operation.method.upper()} {operation.path} says nothing: give it a summary"
        )
    parts = [
        page.heading(3, f"{operation.method.upper()} {operation.path}"),
        page.code("sh", curl(operation, spec, base)),
    ]
    if said:
        parts.append(page.prose(said))
    if detail and detail != said:
        parts.append(page.prose(detail))
    parameters = parameters_of(operation, spec)
    if parameters:
        parts.append(page.table(
            ("Parameter", "In", "Required", "What it takes"),
            [
                (
                    f"`{parameter.get('name')}`",
                    str(parameter.get("in") or ""),
                    "yes" if parameter.get("required") is True else "no",
                    what_it_takes(operation, parameter),
                )
                for parameter in parameters
            ],
        ))
    body = body_of(operation, spec)
    if body is not None:
        parts.append(page.heading(4, f"{operation.method.upper()} {operation.path} body"))
        parts.append(schema_table(body, spec))
    responses = written.get("responses") or {}
    if responses:
        parts.append(page.table(
            ("Status", "What it comes back with", "Fields"),
            [
                (
                    str(status),
                    str(response.get("description") or "—"),
                    ", ".join(names_of(response, spec)) or "—",
                )
                for status, response in sorted(responses.items())
                if isinstance(response, dict)
            ],
        ))
    return parts


def parameters_of(operation: Operation, spec: dict[str, Any]) -> list[dict[str, Any]]:
    """The operation's parameters, the path's own first, each named once."""
    item = ((spec.get("paths") or {}).get(operation.path)) or {}
    found: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for written in [*(item.get("parameters") or []), *(operation.written.get("parameters") or [])]:
        parameter = resolved(written, spec)
        if not isinstance(parameter, dict):
            continue
        key = (str(parameter.get("name") or ""), str(parameter.get("in") or ""))
        if key in seen:
            continue
        seen.add(key)
        found.append(parameter)
    return found


def what_it_takes(operation: Operation, parameter: dict[str, Any]) -> str:
    said = " ".join(str(parameter.get("description") or "").split())
    if not said:
        raise page.ReferenceError(
            f"{operation.method.upper()} {operation.path}: parameter "
            f"{parameter.get('name')!r} has no description"
        )
    kind = written_type(parameter.get("schema") or {})
    return f"{said} ({kind})" if kind not in ("string", "any") else said


def body_of(operation: Operation, spec: dict[str, Any]) -> dict[str, Any] | None:
    body = operation.written.get("requestBody")
    if not isinstance(body, dict):
        return None
    body = resolved(body, spec)
    content = body.get("content") or {}
    chosen = content.get("application/json")
    if not isinstance(chosen, dict):
        chosen = next((value for value in content.values() if isinstance(value, dict)), None)
    if chosen is None:
        return None
    schema = chosen.get("schema")
    return schema if isinstance(schema, dict) else None


def names_of(response: dict[str, Any], spec: dict[str, Any]) -> list[str]:
    """The fields a response names, its object's own and one level down."""
    content = resolved(response, spec).get("content") or {}
    chosen = content.get("application/json")
    if not isinstance(chosen, dict):
        return []
    schema = resolved(chosen.get("schema") or {}, spec)
    properties = schema.get("properties")
    if not isinstance(properties, dict):
        return []
    return [str(name) for name in properties]


def schema_table(schema: dict[str, Any], spec: dict[str, Any]) -> str:
    """A schema's fields as a table, nested objects under their parent's name."""
    rows = fields(schema, spec)
    if not rows:
        return page.prose(f"The body is {written_type(schema)}.")
    return page.table(("Field", "Type", "Required", "What it is"), rows)


def fields(
    schema: dict[str, Any],
    spec: dict[str, Any],
    prefix: str = "",
    depth: int = 0,
) -> list[tuple[str, ...]]:
    schema = resolved(schema, spec)
    properties = schema.get("properties")
    if not isinstance(properties, dict):
        return []
    required = set(schema.get("required") or [])
    rows: list[tuple[str, ...]] = []
    for name, field in properties.items():
        if not isinstance(field, dict):
            continue
        field = resolved(field, spec)
        rows.append((
            f"`{prefix}{name}`",
            written_type(field),
            "yes" if name in required else "no",
            " ".join(str(field.get("description") or "").split()) or "—",
        ))
        if depth < 2 and field.get("type") == "object":
            rows.extend(fields(field, spec, f"{prefix}{name}.", depth + 1))
        items = field.get("items")
        if depth < 2 and field.get("type") == "array" and isinstance(items, dict):
            rows.extend(fields(items, spec, f"{prefix}{name}[].", depth + 1))
    return rows


def written_type(schema: dict[str, Any]) -> str:
    choices = schema.get("enum")
    if isinstance(choices, list) and choices:
        return " or ".join(f"`{choice}`" for choice in choices)
    reference = schema.get("$ref")
    if isinstance(reference, str):
        return reference.rsplit("/", 1)[-1]
    kind = schema.get("type")
    if isinstance(kind, list):
        return " or ".join(str(one) for one in kind)
    if kind == "array":
        items = schema.get("items")
        return f"array of {written_type(items)}" if isinstance(items, dict) else "array"
    if kind == "object" and isinstance(schema.get("properties"), dict):
        return "object"
    return str(kind or "any")


def resolved(written: Any, spec: dict[str, Any]) -> dict[str, Any]:
    """A `$ref` followed, so a table shows the shape rather than the name."""
    node = written
    for _ in range(16):
        if not isinstance(node, dict):
            return {}
        reference = node.get("$ref")
        if not isinstance(reference, str):
            return node
        if not reference.startswith("#/"):
            return node
        target: Any = spec
        for step in reference[2:].split("/"):
            target = target.get(step) if isinstance(target, dict) else None
        node = target
    return {}


def curl(operation: Operation, spec: dict[str, Any], base: str) -> str:
    """A call a person can run: the URL, the key, and the body it takes."""
    query = [
        parameter
        for parameter in parameters_of(operation, spec)
        if parameter.get("in") == "query" and parameter.get("required") is True
    ]
    url = f"{base}{operation.path}"
    if query:
        pairs = "&".join(f"{parameter['name']}={placeholder(parameter.get('schema') or {})}" for parameter in query)
        url = f"{url}?{pairs}"
    lines = [f'curl -X {operation.method.upper()} "{url}" \\']
    lines.append(f'  -H "Authorization: Bearer ${KEY}"')
    body = body_of(operation, spec)
    if body is not None:
        lines[-1] = f"{lines[-1]} \\"
        lines.append('  -H "Content-Type: application/json" \\')
        example = json.dumps(example_of(body, spec), sort_keys=False)
        lines.append(f"  -d '{example}'")
    return "\n".join(lines)


def example_of(schema: dict[str, Any], spec: dict[str, Any]) -> Any:
    """A body with its required fields, each a stand-in of its type."""
    schema = resolved(schema, spec)
    properties = schema.get("properties")
    if not isinstance(properties, dict):
        return {}
    return {
        name: placeholder(field)
        for name, field in properties.items()
        if name in set(schema.get("required") or []) and isinstance(field, dict)
    }


def placeholder(schema: dict[str, Any]) -> Any:
    kind = schema.get("type")
    if "example" in schema:
        return schema["example"]
    if isinstance(schema.get("enum"), list) and schema["enum"]:
        return schema["enum"][0]
    if kind == "integer" or kind == "number":
        return 0
    if kind == "boolean":
        return False
    if kind == "array":
        return []
    if kind == "object":
        return {}
    return "text"


if __name__ == "__main__":
    sys.exit(page.main(
        generate,
        __doc__ or "Write a REST reference from an OpenAPI description.",
        (("--spec", "the OpenAPI description to read, when the tree holds more than one"),),
    ))
