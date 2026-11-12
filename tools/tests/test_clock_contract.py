import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_clock_contract.py"
SPEC = importlib.util.spec_from_file_location("check_clock_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)

OWNER = "crates/collab/tilo-peer-state"
SYNC = "crates/session/session/src/sync"


class ClockContractTests(unittest.TestCase):
    def fixture(self, files: dict[str, str]) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return root

    def paths(self, root: Path) -> list[str]:
        return [Path(violation.path).as_posix() for violation in contract.audit(root)]

    def test_every_direct_read_of_a_clock_in_a_trust_bearing_crate_is_reported(self):
        for phase, source in (
            ("wall", "pub fn at() -> std::time::SystemTime { SystemTime::now() }\n"),
            ("monotonic", "pub fn deadline() -> std::time::Instant { Instant::now() }\n"),
            ("qualified", "pub fn at() -> std::time::SystemTime {\n    std::time::SystemTime::now()\n}\n"),
            ("web", "pub fn at() -> web_time::SystemTime { web_time::SystemTime::now() }\n"),
        ):
            with self.subTest(phase=phase):
                root = self.fixture({f"{OWNER}/src/session.rs": source})
                self.assertEqual(self.paths(root), [f"{OWNER}/src/session.rs"])

    def test_the_clock_the_host_lends_and_a_test_holds_is_the_one_to_read(self):
        root = self.fixture(
            {
                f"{OWNER}/src/session.rs": (
                    "pub fn at() -> tilo_clock::SystemTime { tilo_clock::wall() }\n"
                    "pub fn deadline() -> tilo_clock::Instant { tilo_clock::now() }\n"
                ),
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_tests_and_clock_arithmetic_are_not_reads(self):
        root = self.fixture(
            {
                f"{OWNER}/src/session_tests.rs": "fn at() { SystemTime::now(); }\n",
                f"{OWNER}/src/tests/case.rs": "fn at() { Instant::now(); }\n",
                f"{OWNER}/src/session.rs": (
                    "pub fn shifted(at: std::time::SystemTime) -> std::time::SystemTime {\n"
                    "    at + std::time::Duration::from_secs(60)\n"
                    "}\n"
                    "#[cfg(test)]\n"
                    "mod tests {\n"
                    "    fn at() -> std::time::SystemTime { std::time::SystemTime::now() }\n"
                    "}\n"
                ),
            }
        )
        self.assertEqual(contract.audit(root), [])

    def test_tokio_timers_keep_their_reason_and_every_other_file_needs_one(self):
        listed = "crates/hosting/tilo-room-host/src/peer/delivery.rs"
        with self.subTest(phase="a listed timer file passes"):
            root = self.fixture(
                {
                    f"{listed}": "fn go(deadline: tokio::time::Instant) -> bool {\n"
                    "    tokio::time::Instant::now() < deadline\n"
                    "}\n",
                }
            )
            self.assertEqual(contract.audit(root), [])
        with self.subTest(phase="an unlisted timer read is reported"):
            unlisted = "crates/hosting/tilo-room-host/src/peer/other.rs"
            root = self.fixture({unlisted: "fn go() -> tokio::time::Instant { tokio::time::Instant::now() }\n"})
            self.assertEqual(self.paths(root), [unlisted])
        with self.subTest(phase="a bare timer read is told apart by its import"):
            bare = "crates/hosting/tilo-room-host/src/peer/other.rs"
            root = self.fixture(
                {bare: "use tokio::time::Instant;\nfn go() -> Instant { Instant::now() }\n"}
            )
            self.assertEqual(self.paths(root), [bare])
            root = self.fixture(
                {
                    f"{listed}": "use tokio::time::Instant;\nfn go() -> Instant { Instant::now() }\n",
                }
            )
            self.assertEqual(contract.audit(root), [])

    def test_the_sync_door_is_judged_and_other_crates_are_not(self):
        root = self.fixture(
            {
                f"{SYNC}/client/adopt.rs": "fn at() { std::time::SystemTime::now(); }\n",
                "crates/session/session/src/other.rs": "fn at() { std::time::SystemTime::now(); }\n",
                "crates/collab/collaboration/src/broker.rs": "fn at() { SystemTime::now(); }\n",
            }
        )
        self.assertEqual(self.paths(root), [f"{SYNC}/client/adopt.rs"])

    def test_every_exception_names_a_reason(self):
        for path, reason in contract.TIMERS.items():
            with self.subTest(path=str(path)):
                self.assertTrue(reason.strip())
                self.assertIn("timer", reason)


if __name__ == "__main__":
    unittest.main()
