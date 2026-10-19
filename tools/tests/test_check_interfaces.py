import importlib.util
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

from git_fixture import isolated_git_environment

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_interfaces", TOOLS / "policy/check_interfaces.py"
)
interfaces = importlib.util.module_from_spec(spec)
sys.modules["check_interfaces"] = interfaces
spec.loader.exec_module(interfaces)

SOURCE = '''
//! Wire types.
use serde::{Deserialize, Serialize};

pub const PROTOCOL: u32 = 3;

/// A message, with a `}` in its doc comment.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello { version: u32, #[serde(default)] name: Option<String> },
    Frame(Frame),
    #[serde(rename = "bye")]
    GoodBye,
    #[serde(skip)]
    Internal(u8),
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct Frame {
    pub payload_bytes: Vec<u8>,
    #[serde(with = "serde_bytes")]
    signature: [u8; 64],
    kind: crate::kinds::Kind<'static>,
}

pub type Kind<'a> = Shape;


impl Frame {
    pub fn new() -> Self { todo!("{ not a type }") }
}

pub const fn ahead(theirs: u32) -> bool { theirs > PROTOCOL }

#[derive(Serialize, Deserialize)]
pub enum Shape { Round, SemanticV1 }

#[cfg(test)]
mod tests {
    pub struct Frame { old: u8 }
}
'''


def schema_from(source, roots=("Message",), encoding="json", opaque=None):
    with tempfile.TemporaryDirectory() as root:
        root = Path(root)
        (root / "wire.rs").write_text(source)
        spec = interfaces.Spec(
            name="test", version=3, version_const="wire.rs:PROTOCOL", encoding=encoding,
            sources=["wire.rs"], roots=list(roots), capnp=[], opaque=opaque or {},
            serves=False, path=root / "interface.toml",
        )
        return interfaces.schema_of(spec, root), interfaces.code_version(spec, root)


class SchemaTests(unittest.TestCase):
    def test_the_schema_is_what_the_wire_carries(self):
        schema, version = schema_from(SOURCE)
        self.assertEqual(version, 3)
        self.assertEqual(schema, {
            "Frame": "struct deny_unknown_fields",
            "Frame.kind": "Kind",
            "Frame.payload-bytes": "Vec<u8>",
            "Frame.signature": "[u8; 64] with=serde_bytes",
            "Kind": "alias Shape",
            "Message": "enum internal tag=type",
            "Message::bye": "unit",
            "Message::frame": "tuple(Frame)",
            "Message::hello": "struct",
            "Message::hello.name": "Option<String> optional",
            "Message::hello.version": "u32",
            "Shape": "enum external",
            "Shape::Round": "unit",
            "Shape::SemanticV1": "unit",
        })

    def test_postcard_records_the_order_that_is_its_encoding(self):
        schema, _ = schema_from(SOURCE, encoding="postcard")
        self.assertEqual(schema["Message::frame"], "#1 tuple(Frame)")
        self.assertEqual(schema["Frame.signature"], "#1 [u8; 64] with=serde_bytes")
        self.assertNotIn("optional", schema["Message::hello.name"])

    def test_a_type_nothing_defines_must_be_named_opaque(self):
        source = "pub const PROTOCOL: u32 = 1;\npub struct Root { at: chrono::DateTime<Utc> }"
        with self.assertRaisesRegex(interfaces.InterfaceError, "`DateTime`.*`Utc`"):
            schema_from(source, roots=("Root",))
        schema, _ = schema_from(
            source, roots=("Root",), opaque={"DateTime": "a time", "Utc": "its zone"}
        )
        self.assertEqual(schema["Root.at"], "DateTime<Utc>")

    def test_serde_renames_like_serde(self):
        self.assertEqual(interfaces.renamed("MediaPositionV1", "kebab-case", variant=True),
                         "media-position-v1")
        self.assertEqual(interfaces.renamed("HTTPServer", "snake_case", variant=True),
                         "h_t_t_p_server")
        self.assertEqual(interfaces.renamed("splash_budget", "kebab-case"), "splash-budget")
        self.assertEqual(interfaces.renamed("splash_budget", "camelCase"), "splashBudget")

    def test_capnp_members_carry_their_ordinals(self):
        with tempfile.TemporaryDirectory() as root:
            Path(root, "s.capnp").write_text(textwrap.dedent('''
                @0xdeadbeefdeadbeef;
                # A comment with { braces }.
                interface Store {
                  get @0 (key :Text) -> (value :Data);
                }
                struct Entry {
                  key @0 :Text;
                  union { none @1 :Void; size @2 :UInt64; }
                  kind @3 :Kind;
                  enum Kind { file @0; dir @1; }
                }
            '''))
            schema = interfaces.capnp_schema("s.capnp", Path(root))
        self.assertEqual(schema["capnp:s.capnp:Store.get"], "@0 (key:Text)->(value:Data)")
        self.assertEqual(schema["capnp:s.capnp:Entry.size"], "@2 :UInt64 union")
        self.assertEqual(schema["capnp:s.capnp:Entry.key"], "@0 :Text")
        self.assertEqual(schema["capnp:s.capnp:Entry.Kind.dir"], "@1")


class CompatibilityTests(unittest.TestCase):
    BEFORE = {
        "S": "struct",
        "S.a": "u32",
        "E": "enum internal tag=type",
        "E::one": "unit",
    }

    def test_an_added_optional_field_is_compatible_in_json(self):
        now = dict(self.BEFORE, **{"S.b": "Option<u8> optional"})
        self.assertEqual(interfaces.incompatible(self.BEFORE, now, "json"), [])

    def test_a_new_type_is_judged_where_it_is_used(self):
        now = dict(self.BEFORE, **{"S.b": "Option<T> optional", "T": "struct", "T.x": "u8"})
        self.assertEqual(interfaces.incompatible(self.BEFORE, now, "json"), [])

    def test_every_other_change_breaks(self):
        cases = {
            "required field": dict(self.BEFORE, **{"S.b": "u8"}),
            "new message": dict(self.BEFORE, **{"E::two": "unit"}),
            "type changed": dict(self.BEFORE, **{"S.a": "u64"}),
            "field gone": {k: v for k, v in self.BEFORE.items() if k != "S.a"},
        }
        for case, now in cases.items():
            with self.subTest(case):
                self.assertTrue(interfaces.incompatible(self.BEFORE, now, "json"))
        strict = dict(self.BEFORE, **{"S": "struct deny_unknown_fields"})
        self.assertTrue(interfaces.incompatible(
            strict, dict(strict, **{"S.b": "Option<u8> optional"}), "json"))
        self.assertTrue(interfaces.incompatible(
            self.BEFORE, dict(self.BEFORE, **{"S.b": "#1 Option<u8>"}), "postcard"))

    def test_capnp_fields_may_be_added_and_methods_and_union_members_may_not(self):
        before = {"capnp:s.capnp:Entry": "struct", "capnp:s.capnp:Store": "interface"}
        field = dict(before, **{"capnp:s.capnp:Entry.size": "@1 :UInt64"})
        member = dict(before, **{"capnp:s.capnp:Entry.size": "@1 :UInt64 union"})
        method = dict(before, **{"capnp:s.capnp:Store.put": "@1 (key:Text)->()"})
        self.assertEqual(interfaces.incompatible(before, field, "postcard"), [])
        self.assertTrue(interfaces.incompatible(before, member, "postcard"))
        self.assertTrue(interfaces.incompatible(before, method, "postcard"))


class AgainstMainTests(unittest.TestCase):
    def git(self, root, *args):
        subprocess.run(["git", *args], cwd=root, check=True, capture_output=True,
                       env=isolated_git_environment())

    SPEC = (
        'name = "demo"\nversion = 0\nversion_const = "crates/demo/wire/src/lib.rs:V"\n'
        'encoding = "json"\nsources = ["crates/demo/wire/src/lib.rs"]\nroots = ["S"]\n'
    )

    def source(self, version, fields):
        return f"pub const V: u32 = {version};\npub struct S {{ {fields} }}\n"

    def test_an_incompatible_change_needs_a_new_version(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            self.git(root, "init", "-q", "-b", "main")
            self.git(root, "config", "user.email", "t@example.com")
            self.git(root, "config", "user.name", "t")
            spec = root / "crates/demo/wire/interface.toml"
            code = root / "crates/demo/wire/src/lib.rs"
            code.parent.mkdir(parents=True)
            spec.write_text(self.SPEC)
            code.write_text(self.source(1, "a: u32"))
            self.git(root, "add", ".")
            self.git(root, "commit", "-qm", "base")

            code.write_text(self.source(1, "a: u32, b: u8"))
            problems = interfaces.against("main", root)
            self.assertEqual(len(problems), 1)
            self.assertIn("`S.b` is new", problems[0])

            code.write_text(self.source(2, "a: u32, b: u8"))
            self.assertEqual(interfaces.against("main", root), [])

            code.write_text(self.source(1, "a: u32, b: Option<u8>"))
            self.assertEqual(interfaces.against("main", root), [])

            # Its number moved to another crate: still compared, by name.
            moved = root / "crates/demo/other/interface.toml"
            moved_code = root / "crates/demo/other/src/lib.rs"
            moved_code.parent.mkdir(parents=True)
            moved.write_text(self.SPEC.replace("demo/wire", "demo/other"))
            moved_code.write_text(self.source(1, "a: u64"))
            spec.unlink()
            self.assertIn("`S.a` is now `u64`", interfaces.against("main", root)[0])


class RepositoryTests(unittest.TestCase):
    def test_every_committed_interface_matches_its_code(self):
        self.assertEqual(interfaces.check(), [])

    def test_the_four_interfaces_each_have_one_number(self):
        names = sorted(
            interfaces.load_spec(path)[0].name for path in interfaces.interface_files()
        )
        self.assertEqual(names, ["package", "peer", "runtime", "shell-ui"])


if __name__ == "__main__":
    unittest.main()
