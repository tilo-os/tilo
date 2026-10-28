"""tools/ci/t_matrix.py expands t.toml's matrices as t does (t's README, "Matrices")."""
from __future__ import annotations

from pathlib import Path
import sys
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools/ci"))
import t_matrix  # noqa: E402


def expanded(text: str, paths: list[str] | None = None) -> list[dict]:
    tree = (lambda pattern: t_matrix.from_values(pattern, paths)) if paths is not None else None
    return t_matrix.expand(tomllib.loads(text), tree)["check"]


class TMatrixTests(unittest.TestCase):
    def test_each_combination_is_a_check_with_its_values_in_every_string(self):
        checks = expanded('''
[[check]]
name = "size/{app}-{arch}"
run = "size.sh {arch} {app} ${HOME} {commit}"
inputs = ["apps/{app}/**", "{a,b}.sh"]
requires = ["arch={node}"]
env = { "OUT_{arch}" = "/target/{app}" }
[check.matrix]
arch = [{ arch = "x86_64", node = "x86_64" }, { arch = "aarch64", node = "arm64" }]
app = ["notes", "mail"]
''')
        self.assertEqual([c["name"] for c in checks],
                         ["size/notes-x86_64", "size/notes-aarch64", "size/mail-x86_64", "size/mail-aarch64"])
        self.assertEqual(checks[1], {"name": "size/notes-aarch64", "run": "size.sh aarch64 notes ${HOME} {commit}",
                                     "inputs": ["apps/notes/**", "{a,b}.sh"], "requires": ["arch=arm64"],
                                     "env": {"OUT_aarch64": "/target/notes"}})

    def test_exclude_include_and_set_shape_the_combinations(self):
        checks = expanded('''
[[check]]
name = "build-{os}-{arch}"
run = "build {os} {arch}"
[check.matrix]
os = ["linux", "macos"]
arch = ["x86_64", "aarch64"]
exclude = [{ os = "macos", arch = "x86_64" }]
include = [{ os = "linux", arch = "riscv64" }]
[[check.matrix.set]]
when = { os = "macos" }
requires = ["os=macos"]
''')
        self.assertEqual([c["name"] for c in checks],
                         ["build-linux-x86_64", "build-linux-aarch64", "build-macos-aarch64", "build-linux-riscv64"])
        self.assertEqual(checks[2]["requires"], ["os=macos"])
        self.assertNotIn("requires", checks[0])

    def test_from_takes_values_from_the_paths_of_the_tree(self):
        paths = ["apps/a-b/app.toml", "apps/a/app.toml", "apps/a/src/main.rs", "apps/c/src/main.rs",
                 "apps/top.toml", "apps/d/nested/app.toml", "crates/x/app.toml"]
        self.assertEqual(t_matrix.from_values("apps/*/app.toml", paths), ["a", "a-b"])
        self.assertEqual(t_matrix.from_values("apps/*", paths), ["a", "a-b", "c", "d", "top.toml"])
        self.assertEqual(t_matrix.from_values("apps/*/src", paths), ["a", "c"])
        self.assertEqual(t_matrix.from_values("apps/*.toml", paths), ["top"])
        for bad in ("apps/**/app.toml", "apps/*/*.toml", "apps/{a,b}/*", "../*", "apps"):
            with self.assertRaises(t_matrix.MatrixError):
                t_matrix.from_values(bad, paths)
        text = "[[check]]\nname = 'lint/{app}'\nrun = 'lint {app}'\n[check.matrix]\napp = { from = 'apps/*/app.toml' }\n"
        self.assertEqual([c["name"] for c in expanded(text, paths)], ["lint/a", "lint/a-b"])

    def test_mistakes_are_refused(self):
        for matrix in ("exclude = [{ b = '1' }]\na = ['1']", "a = [{ a = '1', n = '2' }, { a = '2' }]",
                       "a = ['1']\n[[check.matrix.set]]\nrequires = []", "exclude = []", "a = 3",
                       "a = ['1']\nb = [{ b = '1', a = '2' }]"):
            with self.subTest(matrix=matrix), self.assertRaises(t_matrix.MatrixError):
                expanded(f"[[check]]\nname = 'x-{{a}}'\nrun = 'true'\n[check.matrix]\n{matrix}\n")

    def test_t_toml_names_each_check_once(self):
        names = [c["name"] for c in t_matrix.checks(ROOT)]
        self.assertEqual(len(names), len(set(names)))
        self.assertFalse([n for n in names if "{" in n])


if __name__ == "__main__":
    unittest.main()
