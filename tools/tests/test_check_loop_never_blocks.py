import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_loop_never_blocks.py"
SPEC = importlib.util.spec_from_file_location("check_loop_never_blocks", SCRIPT)
check = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = check
SPEC.loader.exec_module(check)


# A pane's model built in place when its background read is missing: the window's thread waits
# for the session's first reading.
READ_IN_PLACE = """\
impl Area {
    pub fn model(&self) -> &Model {
        self.model.get_or_init(|| {
            let read = self.read.take().unwrap_or_else(|| Read::of());
            Model::from(read)
        })
    }
}

impl Read {
    fn of() -> Self {
        Self { devices: SessionResource::open("session:devices").ok() }
    }
}
"""

OFF_THE_WINDOW = """\
fn watch(wake: WakeHandle) {
    std::thread::spawn(move || {
        ui::rt::off_ui_thread();
        let opened = SessionResource::open("session:keyboard");
        drop(opened);
    });
}

fn follow(wake: WakeHandle) -> Option<SessionResource> {
    SessionResource::follow("session:devices", wake).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn opens() {
        let _ = SessionResource::open("session:devices");
    }
}
"""


def audit(files: dict[str, str]) -> list[str]:
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        for relative, text in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return check.audit(root)


class WindowWaitsTest(unittest.TestCase):
    def test_an_open_a_window_may_run_is_named(self) -> None:
        found = audit({"apps/settings/src/panes/mesh.rs": READ_IN_PLACE})
        self.assertEqual(len(found), 1, found)
        self.assertTrue(found[0].startswith("apps/settings/src/panes/mesh.rs:12:"), found)

    def test_a_surface_of_the_session_is_window_code(self) -> None:
        found = audit({"crates/session/lock-screen/src/lock.rs": READ_IN_PLACE})
        self.assertEqual(len(found), 1, found)

    def test_an_open_marked_off_the_window_a_follow_and_a_test_pass(self) -> None:
        self.assertEqual(audit({"apps/osk/src/session.rs": OFF_THE_WINDOW}), [])

    def test_the_session_loop_still_never_blocks_on(self) -> None:
        found = audit({"crates/session/session/src/loop.rs": "fn f() { pollster::block_on(x) }\n"})
        self.assertEqual(len(found), 1, found)


if __name__ == "__main__":
    unittest.main()
