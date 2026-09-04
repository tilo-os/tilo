import importlib.util
import json
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).resolve().parents[1] / "qos-mcp-server.py"
SPEC = importlib.util.spec_from_file_location("qos_mcp_server", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
SERVER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SERVER)


class FakeCompositor:
    def __init__(self):
        self.calls = []

    def action_status(self, invocation_id):
        self.calls.append(("status", invocation_id))
        return {"receipt": {"broker_invocation_id": invocation_id, "update": "accepted"}}

    def cancel_action(self, invocation_id):
        self.calls.append(("cancel", invocation_id))
        return {"receipt": {"broker_invocation_id": invocation_id, "update": "cancelled"}}

    def enumerate_actions(self, query=""):
        self.calls.append(("enumerate", query))
        return {
            "actions": [
                {
                    "provider": "live apps",
                    "id": "files.entry:/Brief.md#rename",
                    "kind": "Action",
                    "title": "Rename Brief.md",
                    "subtitle": "Rename this shared file",
                    "params": [{"name": "name", "type": "text"}],
                }
            ]
        }


class ReceiptToolsTest(unittest.TestCase):
    def test_status_and_cancel_forward_the_exact_broker_receipt_id(self):
        compositor = FakeCompositor()
        for tool, operation in (
            ("qos_action_status", "status"),
            ("qos_cancel_action", "cancel"),
        ):
            result = SERVER.call_tool(compositor, tool, {"invocation_id": 81})
            self.assertFalse(result["isError"])
            payload = json.loads(result["content"][0]["text"])
            self.assertEqual(payload["receipt"]["broker_invocation_id"], 81)
            self.assertEqual(compositor.calls[-1], (operation, 81))

    def test_invalid_receipt_ids_never_reach_the_compositor(self):
        compositor = FakeCompositor()
        for invalid in (None, True, 0, -1, "81"):
            result = SERVER.call_tool(
                compositor, "qos_action_status", {"invocation_id": invalid}
            )
            self.assertTrue(result["isError"])
        self.assertEqual(compositor.calls, [])

    def test_receipt_tools_are_discoverable(self):
        names = {tool["name"] for tool in SERVER.TOOLS}
        self.assertIn("qos_action_status", names)
        self.assertIn("qos_cancel_action", names)


class ResourceProjectionTest(unittest.TestCase):
    def test_action_descriptors_are_standard_mcp_resources(self):
        compositor = FakeCompositor()
        response = SERVER.handle(
            compositor, {"jsonrpc": "2.0", "id": 1, "method": "resources/list"}
        )
        resource = response["result"]["resources"][0]
        self.assertEqual(resource["mimeType"], "application/vnd.qos.action+json")
        self.assertNotIn(" ", resource["uri"])
        read = SERVER.handle(
            compositor,
            {
                "jsonrpc": "2.0",
                "id": 2,
                "method": "resources/read",
                "params": {"uri": resource["uri"]},
            },
        )
        descriptor = json.loads(read["result"]["contents"][0]["text"])
        self.assertEqual(descriptor["id"], "files.entry:/Brief.md#rename")
        self.assertGreaterEqual(compositor.calls.count(("enumerate", "")), 2)

    def test_stale_or_unpermitted_resource_fails_closed(self):
        response = SERVER.handle(
            FakeCompositor(),
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "resources/read",
                "params": {"uri": "qos://action/live%20apps/not-present"},
            },
        )
        self.assertEqual(response["error"]["code"], -32002)

    def test_initialize_announces_resource_support(self):
        response = SERVER.handle(
            FakeCompositor(),
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "initialize",
                "params": {"protocolVersion": "2025-06-18"},
            },
        )
        self.assertTrue(response["result"]["capabilities"]["resources"]["listChanged"])


if __name__ == "__main__":
    unittest.main()
