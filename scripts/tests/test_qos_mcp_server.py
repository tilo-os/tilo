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


if __name__ == "__main__":
    unittest.main()
