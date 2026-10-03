import importlib.util
import json
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).resolve().parents[1] / "tilo-mcp-server.py"
SPEC = importlib.util.spec_from_file_location("tilo_mcp_server", MODULE_PATH)
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
                    "provider": "app-actions",
                    "id": "opaque-rename-address",
                    "kind": "Action",
                    "title": "Rename",
                    "subtitle": "Rename this shared file",
                    "params": [{"name": "name", "type": "text"}],
                    "application": "so.tilo.files",
                    "room": "room-7",
                    "record": "file:/Brief.md",
                    "resource": "files.entry:/Brief.md",
                    "operation": "rename",
                },
                {
                    "provider": "app-actions",
                    "id": "opaque-delete-address",
                    "kind": "Action",
                    "title": "Delete",
                    "subtitle": "Move this shared file to trash",
                    "params": [],
                    "application": "so.tilo.files",
                    "room": "room-7",
                    "record": "file:/Brief.md",
                    "resource": "files.entry:/Brief.md",
                    "operation": "delete",
                },
                {
                    "provider": "actions",
                    "id": "settings.open",
                    "kind": "Actions",
                    "title": "Open settings",
                    "subtitle": None,
                    "params": [],
                },
            ]
        }


class ReceiptToolsTest(unittest.TestCase):
    def test_receipt_tools_are_discoverable_and_forward_only_valid_broker_ids(self):
        with self.subTest(phase='receipt tools are discoverable'):
            names = {tool["name"] for tool in SERVER.TOOLS}
            self.assertIn("tilo_action_status", names)
            self.assertIn("tilo_cancel_action", names)
        with self.subTest(phase='status and cancel forward the exact broker receipt id'):
            compositor = FakeCompositor()
            for tool, operation in (
                ("tilo_action_status", "status"),
                ("tilo_cancel_action", "cancel"),
            ):
                result = SERVER.call_tool(compositor, tool, {"invocation_id": 81})
                self.assertFalse(result["isError"])
                payload = json.loads(result["content"][0]["text"])
                self.assertEqual(payload["receipt"]["broker_invocation_id"], 81)
                self.assertEqual(compositor.calls[-1], (operation, 81))
        with self.subTest(phase='invalid receipt ids never reach the compositor'):
            compositor = FakeCompositor()
            for invalid in (None, True, 0, -1, "81"):
                result = SERVER.call_tool(
                    compositor, "tilo_action_status", {"invocation_id": invalid}
                )
                self.assertTrue(result["isError"])
            self.assertEqual(compositor.calls, [])


class ResourceProjectionTest(unittest.TestCase):
    def _read(self, compositor, request_id, uri):
        response = SERVER.handle(
            compositor,
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "resources/read",
                "params": {"uri": uri},
            },
        )
        return json.loads(response["result"]["contents"][0]["text"])

    def test_stale_unpermitted_or_revoked_resources_fail_closed(self):
        with self.subTest(phase='stale or unpermitted resource fails closed'):
            response = SERVER.handle(
                FakeCompositor(),
                {
                    "jsonrpc": "2.0",
                    "id": 3,
                    "method": "resources/read",
                    "params": {"uri": "tilo://application/not-present"},
                },
            )
            self.assertEqual(response["error"]["code"], -32002)
        with self.subTest(phase='revoked nested action disappears when the application is read again'):
            class RevokingCompositor(FakeCompositor):
                revoked = False

                def enumerate_actions(self, query=""):
                    payload = super().enumerate_actions(query)
                    if self.revoked:
                        payload["actions"] = [
                            action
                            for action in payload["actions"]
                            if action["id"] != "opaque-rename-address"
                        ]
                    return payload

            compositor = RevokingCompositor()
            root = next(
                resource
                for resource in SERVER.list_resources(compositor)["resources"]
                if resource["name"] == "so.tilo.files"
            )
            application = self._read(compositor, 7, root["uri"])
            self.assertTrue(
                any(
                    action["operation"] == "rename"
                    for object_ in application["objects"]
                    for action in object_["actions"]
                )
            )
            compositor.revoked = True
            application = self._read(compositor, 8, root["uri"])
            self.assertFalse(
                any(
                    action["operation"] == "rename"
                    for object_ in application["objects"]
                    for action in object_["actions"]
                )
            )

    def test_resources_are_announced_and_revealed_application_then_object_then_action(self):
        with self.subTest(phase='initialize announces resource support'):
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
        with self.subTest(phase='resources are revealed application then object then action'):
            compositor = FakeCompositor()
            response = SERVER.handle(
                compositor, {"jsonrpc": "2.0", "id": 1, "method": "resources/list"}
            )
            roots = response["result"]["resources"]
            self.assertEqual(
                [root["name"] for root in roots], ["actions", "so.tilo.files"]
            )
            self.assertTrue(
                all(root["mimeType"] == SERVER.APPLICATION_MIME for root in roots)
            )
            self.assertFalse(
                any("opaque-rename-address" in json.dumps(root) for root in roots),
                "resources/list must not preload action addresses",
            )

            files_root = next(root for root in roots if root["name"] == "so.tilo.files")
            application = self._read(compositor, 2, files_root["uri"])
            self.assertEqual(application["type"], "application")
            self.assertEqual(len(application["objects"]), 1)
            object_ = application["objects"][0]
            self.assertEqual(object_["resource"], "files.entry:/Brief.md")
            self.assertEqual(
                [action["operation"] for action in object_["actions"]],
                ["rename", "delete"],
            )
            self.assertEqual(
                object_["actions"][0]["params"], [{"name": "name", "type": "text"}]
            )
            self.assertGreaterEqual(compositor.calls.count(("enumerate", "")), 2)
        with self.subTest(phase='non-app palette providers remain reachable without fake hierarchy'):
            compositor = FakeCompositor()
            roots = SERVER.list_resources(compositor)["resources"]
            shell_root = next(root for root in roots if root["name"] == "actions")
            application = self._read(compositor, 5, shell_root["uri"])
            self.assertEqual(application["objects"][0]["resource"], "settings.open")
            self.assertEqual(application["objects"][0]["actions"][0]["operation"], "invoke")


if __name__ == "__main__":
    unittest.main()
