"""A working tree's change takes the layer route only through the allowlist:

    python3 -m unittest scripts/vm-acceptance/test_layer_route.py
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import layer_route  # noqa: E402

ROOT = "/w"


def package(name, path):
    return {"id": f"{name}-id", "name": name, "manifest_path": f"{ROOT}/{path}/Cargo.toml"}


PACKAGES = [package("ui-core", "crates/sdk/ui-core"), package("shell", "crates/session/shell"),
            package("notes", "apps/notes"), package("runtime-dylib", "crates/runtime/runtime-dylib"),
            package("tilo-vmd", "crates/dev/tilo-vmd"), package("tilo-rollback", "crates/system/rollback"),
            package("lonely", "crates/dev/lonely")]
# shell and runtime-dylib use ui-core; runtime-dylib links notes; tilo-vmd and rollback use nothing.
DEPS = {"shell": ["ui-core"], "runtime-dylib": ["ui-core", "notes"], "notes": ["ui-core"]}
METADATA = {
    "workspace_root": ROOT,
    "workspace_members": [p["id"] for p in PACKAGES],
    "packages": PACKAGES,
    "resolve": {"nodes": [{"id": f"{n}-id", "deps": [{"pkg": f"{d}-id"} for d in DEPS.get(n, [])]}
                          for n in (p["name"] for p in PACKAGES)]},
}
MANIFEST = {"component": [
    {"name": "shell", "kind": "userspace", "packages": ["shell"]},
    {"name": "apps+runtime", "kind": "userspace", "packages": ["runtime-dylib"]},
    {"name": "rollback-files", "kind": "boot", "packages": ["tilo-rollback"]},
]}


def route(*paths):
    return layer_route.route(list(paths), METADATA, MANIFEST)


class LayerRouteTest(unittest.TestCase):
    def test_a_shared_crate_swaps_every_component_it_reaches(self):
        self.assertEqual(route("crates/sdk/ui-core/src/lib.rs"), ("layers", ["apps+runtime", "shell"]))

    def test_an_app_reaches_the_runtime_that_links_it(self):
        self.assertEqual(route("apps/notes/src/main.rs"), ("layers", ["apps+runtime"]))

    def test_the_suite_alone_swaps_nothing(self):
        self.assertEqual(route("scripts/vm-acceptance/run.py"), ("layers", []))

    def test_what_the_base_carries_takes_the_full_route(self):
        for path in ("crates/dev/tilo-vmd/src/main.rs", "crates/system/rollback/src/lib.rs", "Cargo.lock",
                     "os/Containerfile.dev", "t.toml", "crates/dev/lonely/src/lib.rs"):
            way, why = route("crates/session/shell/src/a.rs", path)
            self.assertEqual(way, "full", path)
            self.assertIn(path, why)


if __name__ == "__main__":
    unittest.main()
