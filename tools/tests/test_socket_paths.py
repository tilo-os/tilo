import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_socket_paths.py"
SPEC = importlib.util.spec_from_file_location("check_socket_paths", SCRIPT)
socket_paths = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = socket_paths
SPEC.loader.exec_module(socket_paths)


class SocketPaths(unittest.TestCase):
    def audit(
        self, source: str, relative: str = "crates/x/src/lib.rs", allowances: str = ""
    ) -> int:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / relative
            path.parent.mkdir(parents=True)
            path.write_text(source, encoding="utf-8")
            if allowances:
                table = root / "crates/sdk/ui-core/tests/support/allowances.rs"
                table.parent.mkdir(parents=True)
                table.write_text(allowances, encoding="utf-8")
            found, _, self.stale = socket_paths.audit(root)
            return sum(found.values())

    def test_a_bind_or_connect_by_path_is_one(self):
        self.assertEqual(self.audit("fn f(p: &Path) { UnixListener::bind(p); }"), 1)
        self.assertEqual(
            self.audit("async fn f(p: &Path) { tokio::net::UnixStream::connect(p).await; }"),
            1,
        )

    def test_the_helper_and_tests_are_not_counted(self):
        self.assertEqual(
            self.audit("fn f(p: &Path) { UnixListener::bind(p); }", socket_paths.HELPER), 0
        )
        self.assertEqual(
            self.audit("#[cfg(test)]\nmod tests { fn f(p: &Path) { UnixStream::connect(p); } }"),
            0,
        )

    def test_an_allowed_call_is_not_counted(self):
        self.assertEqual(
            self.audit(
                'fn f() { UnixStream::connect("/run/x.sock"); }',
                allowances='allowance("crates/x/src/lib.rs", "f", Rule::SocketPath, "a fixed short path"),',
            ),
            0,
        )
        self.assertEqual(self.stale, [])
        self.assertEqual(self.audit('fn f() { let s = "UnixStream::connect(p)"; }'), 0)

    def test_an_allowance_is_found_for_a_call_below_comments(self):
        self.assertEqual(
            self.audit(
                "// A comment above the call, as most files have.\n"
                "fn f(p: &Path) {\n"
                "    UnixStream::connect(p);\n"
                "}",
                allowances='allowance("crates/x/src/lib.rs", "f", Rule::SocketPath, "a fixed short path"),',
            ),
            0,
        )

    def test_an_allowance_that_covers_nothing_is_stale(self):
        self.assertEqual(
            self.audit(
                "fn f(p: &Path) { UnixStream::connect(p); }",
                allowances='allowance("crates/x/src/lib.rs", "gone", Rule::SocketPath, "a fixed short path"),',
            ),
            1,
        )
        self.assertEqual(self.stale, ["crates/x/src/lib.rs gone"])


if __name__ == "__main__":
    unittest.main()
