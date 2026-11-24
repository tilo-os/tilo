import tempfile
import unittest
from pathlib import Path

from tools.policy.check_test_support_shipping import audit


def write(root: Path, relative: str, text: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


class TestSupportShippingTests(unittest.TestCase):
    def test_the_unconfined_test_start_reaches_no_shipping_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write(
                root,
                "crates/os/daemon/Cargo.toml",
                '[package]\nname = "tilo-daemon"\n\n[features]\ndefault = ["test-support"]\n'
                'test-support = ["tilo-runtime/test-support"]\n\n[dependencies]\n'
                'tilo-runtime = { path = "../tilo-runtime" }\n\n[dev-dependencies]\n'
                'tilo-runtime = { path = "../tilo-runtime", features = ["test-support"] }\n',
            )
            write(
                root,
                "crates/desktop/session/Cargo.toml",
                '[package]\nname = "session"\n\n[dependencies]\n'
                'tilo-runtime = { path = "../tilo-runtime", features = ["test-support"] }\n',
            )
            write(
                root,
                "tools/workspace-unify/Cargo.toml",
                '[package]\nname = "workspace-hack"\n\n'
                "[target.'cfg(all(target_os = \"linux\", target_arch = \"x86_64\"))'.dependencies]\n"
                'tilo-runtime = { path = "../tilo-runtime", features = ["test-support"] }\n'
                "[target.'cfg(windows)'.dependencies]\n"
                'daemon = { package = "tilo-daemon", path = "../daemon", features = ["test-support"] }\n',
            )
            write(
                root,
                "tools/ci/windows/build.sh",
                "# cargo build --features tilo-daemon/test-support in a comment is fine\n"
                "cargo xwin test --no-run -p tilo-socket --all-features\n"
                "cargo xwin build --locked -p tilo-daemon --features tilo-daemon/test-support\n"
                "cargo xwin build --locked --all-features -p tilolang-process\n",
            )
            found = sorted((v.path, v.line, v.what) for v in audit(root))
            self.assertEqual(
                found,
                [
                    ("crates/desktop/session/Cargo.toml", 5,
                     "[dependencies] enables tilo-runtime's test-support"),
                    ("crates/os/daemon/Cargo.toml", 5, "`test-support` is a default feature"),
                    ("tools/ci/windows/build.sh", 3,
                     "a release build enables `tilo-daemon/test-support`"),
                    ("tools/ci/windows/build.sh", 4, "a release build enables `--all-features`"),
                    ("tools/workspace-unify/Cargo.toml", 7,
                     "[target.'cfg(windows)'.dependencies] enables tilo-daemon's test-support"),
                ],
            )


if __name__ == "__main__":
    unittest.main()
