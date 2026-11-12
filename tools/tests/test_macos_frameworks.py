import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "ci/macos-frameworks.py"
SPEC = importlib.util.spec_from_file_location("macos_frameworks", SCRIPT)
macos_frameworks = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = macos_frameworks
SPEC.loader.exec_module(macos_frameworks)


def declarations(source: str):
    """What the stub writer reads out of one crate's source file."""
    text = macos_frameworks.COMMENT.sub("", source)
    attributed, crate_level = macos_frameworks.file_declarations(text)
    symbols = {
        library: {macos_frameworks.stub_symbol(name) for name in names}
        for library, names in attributed.items()
    }
    classes = macos_frameworks.class_symbols(text)
    return symbols, crate_level, classes


class DeclarationTests(unittest.TestCase):
    """Which library a crate's declaration belongs to.

    A wrong attribution links a program the linker is happy with and a Mac refuses to run, so
    each way a crate names its library is pinned here.
    """

    def test_a_block_names_its_library_over_its_own_symbols(self):
        source = """
#[link(name = "Security", kind = "framework")]
extern "C" {
    pub fn SecItemAdd(attributes: CFDictionaryRef) -> OSStatus;
    pub fn SecItemCopyMatching(query: CFDictionaryRef, result: *mut CFTypeRef) -> OSStatus;
}
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    pub fn CFRelease(cf: CFTypeRef);
}
"""
        symbols, crate_level, _ = declarations(source)
        self.assertEqual(symbols["Security"], {"_SecItemAdd", "_SecItemCopyMatching"})
        self.assertEqual(symbols["CoreFoundation"], {"_CFRelease"})
        self.assertEqual(crate_level, [])

    def test_an_empty_block_names_the_whole_crates_library(self):
        # The generated bindings name their framework once and declare in every file under it.
        source = """
#[link(name = "AppKit", kind = "framework")]
extern "C" {}
pub extern "C-unwind" fn NSApplicationLoad() -> bool {
    extern "C-unwind" {
        fn NSApplicationLoad() -> bool;
    }
    unsafe { NSApplicationLoad() }
}
"""
        symbols, crate_level, _ = declarations(source)
        self.assertEqual(symbols, {})
        self.assertEqual(crate_level, ["AppKit"])

    def test_an_attribute_inside_cfg_attr_counts(self):
        source = """
#[cfg_attr(target_vendor = "apple", link(name = "Foundation", kind = "framework"))]
extern "C" {
    pub fn NSLog(format: *const c_char);
}
"""
        symbols, _, _ = declarations(source)
        self.assertEqual(symbols["Foundation"], {"_NSLog"})

    def test_an_extern_macro_is_a_block(self):
        # `objc2` wraps its declarations in `extern_c!` and `extern_c_unwind!`.
        source = """
extern_c_unwind! {
    pub fn objc_enumerationMutation(obj: *mut AnyObject);
}
extern_c! {
    pub fn ivar_getName(ivar: *const Ivar) -> *const c_char;
}
"""
        self.assertEqual(
            macos_frameworks.all_extern_symbols(source),
            {"objc_enumerationMutation", "ivar_getName"},
        )

    def test_a_link_name_is_the_symbol(self):
        # Rust's own `\u{1}`-prefixed link name: an Objective-C class's symbol, not its Rust name.
        escaped = chr(92) + "u{1}_OBJC_CLASS_$_NSString"
        source = (
            """
#[link(name = "Security", kind = "framework")]
extern "C" {
    #[cfg_attr(target_arch = "aarch64", link_name = "SecCertificateCopyPublicKey$LEGACYMAC")]
    pub fn SecCertificateCopyPublicKey(certificate: SecCertificateRef) -> OSStatus;
"""
            + f'    #[link_name = "{escaped}"]\n'
            + """    static NSString: *const c_void;
}
"""
        )
        symbols, _, _ = declarations(source)
        self.assertEqual(
            symbols["Security"],
            {
                "_SecCertificateCopyPublicKey",
                "_SecCertificateCopyPublicKey$LEGACYMAC",
                "_OBJC_CLASS_$_NSString",
            },
        )

    def test_classes_are_the_crates(self):
        source = """
#[link(name = "Metal", kind = "framework")]
extern "C" {}
extern_class!(
    #[derive(Debug)]
    pub struct MTLDevice;
    unsafe impl ClassType for MTLDevice {
        type Super = NSObject;
    }
);
"""
        _, crate_level, classes = declarations(source)
        self.assertEqual(crate_level, ["Metal"])
        self.assertEqual(classes, {"_OBJC_CLASS_$_MTLDevice", "_OBJC_METACLASS_$_MTLDevice"})

    def test_documentation_names_no_library(self):
        # A doc example links CoreGraphics; the file itself is the Metal crate's.
        source = """
//! Or link it yourself:
//! #[link(name = "CoreGraphics", kind = "framework")]
//! extern "C" {}
#[link(name = "Metal", kind = "framework")]
extern "C" {}
"""
        _, crate_level, _ = declarations(source)
        self.assertEqual(crate_level, ["Metal"])

    def test_a_dylib_defaults_to_dylib(self):
        source = """
#[cfg_attr(not(feature = "rustc-dep-of-std"), link(name = "iconv"))]
extern "C" {
    pub fn iconv_open(tocode: *const c_char, fromcode: *const c_char) -> iconv_t;
}
"""
        symbols, _, _ = declarations(source)
        self.assertEqual(symbols["iconv"], {"_iconv_open"})


class StubTests(unittest.TestCase):
    def test_a_stub_names_the_path_dyld_loads_and_its_symbols(self):
        text = macos_frameworks.stub(
            macos_frameworks.INSTALL_NAMES["Foundation"], {"_NSLog", "_NSHomeDirectory"}
        )
        self.assertIn("--- !tapi-tbd", text)
        self.assertIn(
            "install-name:    "
            "'/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation'",
            text,
        )
        self.assertIn("'_NSHomeDirectory', '_NSLog'", "".join(text.split("\n")))
        self.assertTrue(text.endswith("...\n"))

    def test_each_library_writes_its_own_shape_of_stub(self):
        with tempfile.TemporaryDirectory() as out:
            macos_frameworks.write_stubs(
                Path(out),
                {"Foundation": {"_NSLog"}, "objc": {"_objc_msgSend"}, "Carbon": set()},
            )
            foundation = Path(out) / "Foundation.framework/Foundation.tbd"
            self.assertIn("'_NSLog'", foundation.read_text(encoding="utf-8"))
            self.assertIn(
                "install-name:    '/usr/lib/libobjc.A.dylib'",
                (Path(out) / "libobjc.tbd").read_text(encoding="utf-8"),
            )
            # A library the program does not link is no stub, and neither is an empty one.
            self.assertFalse((Path(out) / "Carbon.framework").exists())


if __name__ == "__main__":
    unittest.main()
