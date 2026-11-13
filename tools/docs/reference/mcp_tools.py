#!/usr/bin/env python3
"""Write the reference of the tools an agent gets in a workspace.

The tools are what the workspace's MCP server answers `tools/list` with, read
from the server's own definitions: `tilo-runtime-mcp --tools-json` prints them
from the runtime, and this page renders that answer. A tool the server adds,
renames or rewords reaches the page, and one with no description or no
parameter documentation stops the generator rather than appearing here as a
bare name.

    mcp_tools.py --root <repository> --out <page.md>

`--tools FILE` reads a saved `{"tools": [...]}` answer instead of building the
runtime and asking it.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


sys.path.insert(0, str(Path(__file__).resolve().parent))
import page  # noqa: E402  (beside this script, not on the isolated path)


PACKAGE = "crates/runtime/tilo-runtime"

# The order an agent meets the tools in. A tool no group names is an error, and
# so is a name here that the server no longer serves.
GROUPS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("Workspace tools", (
        "tilo_ls", "tilo_read", "tilo_find", "tilo_link", "tilo_watch",
        "tilo_do", "tilo_status", "tilo",
    )),
    ("Files", ("tilo_put_begin", "tilo_put_append", "tilo_put_finish", "tilo_get")),
    ("Devices", ("tilo_devices",)),
)

LEAD = """\
An agent reaches a workspace through the MCP server a hosted workspace serves at
its URL, with a key minted for that one workspace (`tilo keys mint -w <workspace>`).
The agent lists these tools and calls one by name; every call is a step of the
agent's run, so an app's undo takes it back and history shows who did it.
"""

CALL = """\
{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
 "params": {"name": "tilo_ls", "arguments": {"path": "notes/Todo"}}}
"""


def generate(arguments: argparse.Namespace) -> str:
    tools = tool_list(arguments.root.resolve(), arguments.tools)
    by_name = {tool["name"]: tool for tool in tools}
    placed = [name for _, names in GROUPS for name in names]
    unknown = sorted(set(placed) - set(by_name))
    unplaced = sorted(set(by_name) - set(placed))
    if unknown:
        raise page.ReferenceError(f"groups name tools the server does not serve: {', '.join(unknown)}")
    if unplaced:
        raise page.ReferenceError(f"no group holds: {', '.join(unplaced)}")
    sections = [
        page.prose(
            "A call is JSON-RPC over the session's one POST address, and an answer "
            "arrives the same way:"
        ),
        page.code("json", CALL),
        page.prose(
            "Read the `structuredContent` of the answer for the typed result; a call "
            "that failed says why in `error`, with the calls that would work."
        ),
    ]
    for title, names in GROUPS:
        sections.append(page.heading(2, title))
        for name in names:
            sections.extend(render(by_name[name]))
    return page.page("MCP tools", LEAD, sections)


def tool_list(root: Path, saved: Path | None) -> list[dict[str, Any]]:
    """The tools the server serves, from a saved answer or from the runtime."""
    written = saved.read_text(encoding="utf-8") if saved is not None else printed_tools(root)
    try:
        answer = json.loads(written)
    except json.JSONDecodeError as error:
        raise page.ReferenceError(f"the tool list is not JSON: {error}") from error
    tools = answer.get("tools")
    if not isinstance(tools, list) or not tools:
        raise page.ReferenceError("the server answered no tools")
    return tools


def printed_tools(root: Path) -> str:
    """The tools the runtime prints for a workspace, without a session."""
    result = subprocess.run(
        [
            "cargo", "run", "--locked", "-q", "-p", "tilo-runtime",
            "--bin", "tilo-runtime-mcp", "--", "--tools-json",
        ],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    answer = result.stdout.strip()
    if not answer:
        reason = (result.stderr or result.stdout).strip().splitlines()
        raise page.ReferenceError(
            f"{PACKAGE} printed no tools: "
            + (reason[-1] if reason else f"it exited {result.returncode}")
        )
    return answer


def render(tool: dict[str, Any]) -> list[str]:
    name = tool["name"]
    description = (tool.get("description") or "").strip()
    if not description:
        raise page.ReferenceError(f"{name} has no description: an agent reads that to choose it")
    parts = [page.heading(3, name), page.prose(description)]
    schema = tool.get("inputSchema") or {}
    properties = schema.get("properties") or {}
    required = set(schema.get("required") or [])
    if properties:
        rows: list[tuple[str, ...]] = []
        for option, field in properties.items():
            if not isinstance(field, dict):
                raise page.ReferenceError(f"{name}.{option}: the schema is not an object")
            rows.append((
                f"`{option}`",
                written_type(field),
                "yes" if option in required else "no",
                what_it_takes(field),
            ))
        parts.append(page.table(("Option", "Type", "Required", "What it takes"), rows))
    else:
        parts.append(page.prose("It takes no arguments."))
    marked = hints(tool.get("annotations") or {})
    if marked:
        parts.append(page.prose(marked))
    return parts


def written_type(field: dict[str, Any]) -> str:
    """A parameter's type in words: `string`, `array of string`, `app or desktop`."""
    choices = field.get("enum")
    if isinstance(choices, list) and choices:
        return " or ".join(f"`{choice}`" for choice in choices)
    kind = field.get("type")
    if isinstance(kind, list):
        return " or ".join(str(one) for one in kind)
    items = field.get("items")
    if kind == "array" and isinstance(items, dict):
        return f"array of {written_type(items)}"
    return str(kind or "any")


def what_it_takes(field: dict[str, Any]) -> str:
    description = " ".join(str(field.get("description") or "").split())
    bounds: list[str] = []
    if "minItems" in field:
        bounds.append(f"at least {field['minItems']}")
    if "maxItems" in field:
        bounds.append(f"at most {field['maxItems']}")
    if bounds:
        bounded = ", ".join(bounds) + " items"
        description = f"{description} ({bounded})" if description else bounded
    return description or "—"


def hints(annotations: dict[str, Any]) -> str:
    """What the server says a client may assume about calling it."""
    said: list[str] = []
    if annotations.get("destructiveHint") is True:
        said.append("It changes what is there, so the person may be asked to allow it.")
    if annotations.get("openWorldHint") is True:
        said.append("It reaches beyond the workspace.")
    return " ".join(said)


if __name__ == "__main__":
    sys.exit(page.main(
        generate,
        __doc__ or "Write the reference of the tools an agent gets.",
        (("--tools", "a saved {\"tools\": [...]} answer to render, instead of running the runtime"),),
    ))
