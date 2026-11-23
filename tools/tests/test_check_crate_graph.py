import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_crate_graph.py"
SPEC = importlib.util.spec_from_file_location("check_crate_graph", SCRIPT)
graph = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = graph
SPEC.loader.exec_module(graph)


class DaemonAppsTest(unittest.TestCase):
    def test_a_new_edge_and_a_stale_count_both_fail(self) -> None:
        tree = (
            "0daemon v0 (/r/crates/daemon)\n1session v0 (/r/crates/session)\n2chat v0 (/r/apps/chat)\n"
            "3chat-model v0 (/r/apps/chat-model)\n2tasks v0 (/r/apps/tasks)\n"
            "1render v0 (/r/crates/render)\n2chat v0 (/r/apps/chat) (*)\n"
        )
        edges = graph.app_edges(tree, "/r/apps")
        self.assertEqual(edges, {("session", "chat"), ("session", "tasks"), ("render", "chat")})
        found = graph.daemon_findings(edges, {"session": 1, "render": 2})
        self.assertEqual(len(found), 2)
        self.assertIn("lower it to 1", found[0])
        self.assertIn("session reaches 2 app crates (chat, tasks), more than the 1", found[1])
