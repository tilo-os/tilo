"""tools/docs/reference/mcp_tools.py writes the tools an agent is offered."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "tools/docs/reference/mcp_tools.py"


def tool(
    name: str,
    description: str,
    properties: dict | None = None,
    required: tuple[str, ...] = (),
    annotations: dict | None = None,
    ) -> dict:
    return {
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties or {},
            "required": list(required),
            "additionalProperties": False,
        },
        "annotations": annotations or {},
    }


READ = {"readOnlyHint": True, "openWorldHint": False}
WRITE = {"readOnlyHint": False, "destructiveHint": True, "openWorldHint": True}

TOOLS = [
    tool(
        "tilo_ls",
        "List what is in the workspace.",
        {
            "path": {"type": "string", "description": "The app or resource."},
            "kind": {"enum": ["resources", "actions"]},
        },
        annotations=READ,
    ),
    tool("tilo_read", "Read a resource as text.", {"path": {"type": "string", "description": "What to read."}}, ("path",), READ),
    tool("tilo_find", "Search the workspace by words.", {"query": {"type": "string", "description": "The words."}}, ("query",), READ),
    tool("tilo_link", "A link a person opens.", {"path": {"type": "string", "description": "What to link."}}, ("path",), READ),
    tool("tilo_watch", "What changed since cursor.", {"cursor": {"type": "string", "description": "Where to go on from."}}, (), READ),
    tool("tilo_do", "Run an action.", {"path": {"type": "string", "description": "The action."}}, ("path",), WRITE),
    tool("tilo_status", "How an invocation went.", {"invocation_id": {"type": "string", "description": "The invocation."}}, ("invocation_id",), READ),
    tool("tilo", "Run a tilolang program.", {"program": {"type": "string", "description": "The program."}}, ("program",), WRITE),
    tool("tilo_put_begin", "Upload bytes: begin.", {"size": {"type": "integer", "description": "How many."}}, ("size",), WRITE),
    tool("tilo_put_append", "Append the next chunk.", {"offset": {"type": "integer", "description": "Bytes sent so far."}}, ("offset",), WRITE),
    tool("tilo_put_finish", "Finish an upload.", {}, (), WRITE),
    tool("tilo_get", "Download a file.", {"content": {"type": "string", "description": "The reference."}}, ("content",), READ),
    tool("tilo_devices", "List your other devices.", {}, (), {"readOnlyHint": True, "openWorldHint": True}),
]


class McpToolsReferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.out = self.root / "generated" / "mcp-tools.md"
        self.saved = self.root / "tools.json"

    def tearDown(self) -> None:
        self.directory.cleanup()

    def generate(self, tools: list[dict] | None = None) -> subprocess.CompletedProcess[str]:
        self.saved.write_text(json.dumps({"tools": TOOLS if tools is None else tools}), encoding="utf-8")
        return subprocess.run(
            [
                sys.executable, "-I", str(GENERATOR),
                "--root", str(self.root), "--out", str(self.out), "--tools", str(self.saved),
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def page(self) -> str:
        return self.out.read_text(encoding="utf-8")

    def test_writes_every_tool_with_what_it_takes(self) -> None:
        result = self.generate()
        self.assertEqual(result.returncode, 0, result.stderr)
        page = self.page()
        self.assertTrue(page.startswith("# MCP tools\n"))
        self.assertIn("## Workspace tools\n", page)
        self.assertIn("### tilo_ls\n", page)
        self.assertIn("List what is in the workspace.", page)
        self.assertIn("| `path` | string | no | The app or resource. |", page)
        self.assertIn("| `kind` | `resources` or `actions` | no | — |", page)
        self.assertIn("### tilo_do\n", page)
        self.assertIn(
            "It changes what is there, so the person may be asked to allow it. "
            "It reaches beyond the workspace.",
            page,
        )
        self.assertIn("### tilo_devices\n", page)
        self.assertIn("It takes no arguments.", page)
        self.assertLess(page.index("## Workspace tools"), page.index("## Files"))
        self.assertLess(page.index("## Files"), page.index("## Devices"))

    def test_an_array_says_what_it_holds(self) -> None:
        tools = json.loads(json.dumps(TOOLS))
        tools[0]["inputSchema"]["properties"]["calls"] = {
            "type": "array",
            "items": {"type": "object"},
            "description": "The calls, in order.",
        }
        self.assertEqual(self.generate(tools).returncode, 0)
        self.assertIn("| `calls` | array of object | no | The calls, in order. |", self.page())

    def test_two_runs_write_the_same_page(self) -> None:
        self.assertEqual(self.generate().returncode, 0)
        first = self.page()
        self.assertEqual(self.generate().returncode, 0)
        self.assertEqual(first, self.page())

    def test_a_tool_with_no_description_is_an_error(self) -> None:
        tools = json.loads(json.dumps(TOOLS))
        tools[0]["description"] = ""
        result = self.generate(tools)
        self.assertEqual(result.returncode, 1)
        self.assertIn("tilo_ls has no description", result.stderr)

    def test_a_tool_no_group_holds_is_an_error(self) -> None:
        tools = json.loads(json.dumps(TOOLS))
        tools.append(tool("tilo_extra", "Something new."))
        result = self.generate(tools)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no group holds: tilo_extra", result.stderr)

    def test_a_tool_the_server_stopped_serving_is_an_error(self) -> None:
        tools = [one for one in json.loads(json.dumps(TOOLS)) if one["name"] != "tilo_devices"]
        result = self.generate(tools)
        self.assertEqual(result.returncode, 1)
        self.assertIn("groups name tools the server does not serve: tilo_devices", result.stderr)


if __name__ == "__main__":
    unittest.main()
