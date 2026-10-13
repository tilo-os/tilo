import importlib.util
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_portability.py"
SPEC = importlib.util.spec_from_file_location("check_app_portability", SCRIPT)
portability = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = portability
SPEC.loader.exec_module(portability)

RULES = Path(__file__).resolve().parents[1] / "policy/app_portability_rules.toml"
UI = 'ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app" }\n'


class AppPortabilityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write(
            "Cargo.toml",
            '[workspace]\nmembers = ["apps/*"]\n\n[workspace.dependencies]\n'
            'serde = "1"\nhost-info = { path = "crates/system/host-info" }\n',
        )

    def write(self, relative: str, text: str) -> Path:
        path = self.root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text), encoding="utf-8")
        return path

    def app(
        self,
        name: str,
        source: str = "pub fn run() {}\n",
        *,
        dependencies: str = UI,
        cargo_extra: str = "",
        package_keys: str = "",
        manifest: bool = True,
    ) -> Path:
        self.write(
            f"apps/{name}/Cargo.toml",
            f'[package]\nname = "{name}"\nversion = "0.1.0"\n\n'
            f"[dependencies]\n{dependencies}{cargo_extra}",
        )
        if manifest:
            self.write(
                f"apps/{name}/tilo-app.toml",
                f'[package]\nid = "so.tilo.{name}"\nversion = "0.1.0"\nkind = "app"\n'
                f"{package_keys}",
            )
        self.write(f"apps/{name}/src/lib.rs", source)
        return self.root / "apps" / name

    def findings(self, package: str | None = None):
        found = portability.audit(self.root, RULES)
        return [finding for finding in found if package is None or finding.package == package]

    def rules(self, package: str) -> dict[str, int]:
        counts: dict[str, int] = {}
        for finding in self.findings(package):
            counts[finding.rule] = counts.get(finding.rule, 0) + 1
        return counts

    def test_a_portable_app_is_clean(self):
        self.app("notes", "pub fn run() -> ui::Result<()> { ui::main() }\n")
        self.assertEqual(self.findings(), [])

    def test_target_selection_is_r1_except_the_windows_subsystem(self):
        self.app(
            "clocks",
            """\
            #![cfg_attr(windows, windows_subsystem = "windows")]
            #[cfg(unix)]
            fn a() {}
            fn b() -> bool { cfg!(target_os = "macos") }
            #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
            fn c() {}
            #[cfg(feature = "extra")]
            fn d() {}
            """,
            cargo_extra="\n[target.'cfg(unix)'.dependencies]\nserde = \"1\"\n",
        )
        sites = [(Path(f.path).name, f.line) for f in self.findings() if f.rule == "R1"]
        self.assertEqual(sites, [("Cargo.toml", 8), ("lib.rs", 2), ("lib.rs", 4), ("lib.rs", 5)])

    def test_target_dev_dependencies_reach_only_tests(self):
        self.app(
            "notes",
            "pub fn a() {}\n",
            cargo_extra="\n[target.'cfg(target_os = \"linux\")'.dev-dependencies]\nserde = \"1\"\n",
        )
        self.assertEqual([f for f in self.findings() if f.rule == "R1"], [])

    def test_os_interfaces_are_r2(self):
        self.app(
            "terminal",
            """\
            use std::os::unix::fs::PermissionsExt;
            use std::{fs, os::fd::AsRawFd};
            unsafe extern "C" { fn abs(v: i32) -> i32; }
            pub fn pid() -> i32 { unsafe { libc::getpid() } }
            extern crate alloc;
            """,
            dependencies=UI + 'libc = "0.2"\n',
        )
        r2 = [(f.line, f.what) for f in self.findings() if f.rule == "R2"]
        self.assertEqual([line for line, _ in r2], [1, 2, 3, 4])
        self.assertIn("libc", r2[-1][1])

    def test_platform_paths_and_environment_are_r3_but_not_in_comments(self):
        self.app(
            "chat",
            """\
            // Reads /proc/self/status and $HOME.
            pub fn host() -> String { std::fs::read_to_string("/etc/hostname").unwrap() }
            pub fn home() -> Option<std::ffi::OsString> { std::env::var_os("HOME") }
            pub fn data() -> Option<String> { std::env::var("XDG_DATA_HOME").ok() }
            pub fn lang() -> Option<String> { std::env::var("LANG").ok() }
            pub fn cfg() -> &'static str { "~/.config/chat" }
            pub fn quote() -> char { '"' }
            pub fn raw() -> &'static str { r#"/usr/share/"quoted""# }
            pub fn clip() -> &'static str { "https://demo.local/clip.mp4" }
            pub fn file() -> &'static str { "file:///home/u/.local/x" }
            """,
        )
        self.assertEqual(
            [f.line for f in self.findings() if f.rule == "R3"], [2, 3, 4, 6, 8, 10]
        )

    def test_os_tools_are_r4_only_when_named_by_a_literal(self):
        self.app(
            "containers",
            """\
            use std::process::Command;
            pub fn a() { Command::new("podman").status().ok(); }
            pub fn b(program: &str) { Command::new(program).status().ok(); }
            pub fn c() { let _ = CommandSpec::new( "udisksctl" ); }
            """,
        )
        self.assertEqual([f.line for f in self.findings() if f.rule == "R4"], [2, 4])

    def test_dependencies_are_d1_and_d2_but_dev_dependencies_and_the_sdk_are_not(self):
        self.write("crates/system/host-info/Cargo.toml", '[package]\nname = "host-info"\n')
        self.app(
            "backups",
            dependencies=UI
            + 'libc = "0.2"\n'
            + "host-info.workspace = true\n"
            + 'tilo-actions = { path = "../../crates/collab/actions" }\n'
            + 'tilo-files = { path = "../../crates/platform/tilo-files" }\n'
            + 'windows-sys = "0.59"\n',
            cargo_extra="\n[dev-dependencies]\nnix = \"0.29\"\n"
            'bounded-command = { path = "../../crates/system/bounded-command" }\n',
        )
        found = self.findings()
        self.assertEqual(
            sorted(f.what for f in found if f.rule == "D1"),
            ["depends on `host-info`", "depends on `libc`", "depends on `windows-sys`"],
        )
        self.assertEqual(
            sorted(f.what.split("`")[1] for f in found if f.rule == "D2"),
            ["host-info", "tilo-actions"],
        )

    def test_sdk_backend_features_are_f1(self):
        self.app(
            "music",
            dependencies='ui = { package = "tilo-app", path = "../../crates/sdk/tilo-app", '
            'features = ["portal", "test-support"] }\n',
            cargo_extra='\n[features]\nthin = []\nmedia = ["ui/mpris", "ui/webview"]\n',
        )
        self.assertEqual(
            sorted(f.what for f in self.findings() if f.rule == "F1"),
            ["selects `ui/mpris`", "selects `ui/portal`"],
        )

    def test_web_traps_are_w1_to_w3(self):
        self.app(
            "photos",
            """\
            use std::time::{Instant, SystemTime};
            pub fn a() { std::thread::spawn(|| {}); }
            pub fn b() { std::thread::sleep(std::time::Duration::ZERO); }
            pub fn c() -> Instant { Instant::now() }
            pub fn d() { let _ = SystemTime::now(); }
            pub fn e() { let _ = std::fs::File::open("x"); }
            pub fn f() { let _ = File::create("x"); }
            pub fn g() { let _ = std::net::TcpStream::connect("x"); }
            """,
        )
        rules = self.rules("photos")
        self.assertEqual((rules["W1"], rules["W2"], rules["W3"]), (2, 2, 3))

    def test_test_code_is_exempt(self):
        app = self.app(
            "calendar",
            """\
            mod store;
            #[cfg(test)]
            mod store_tests;
            #[cfg(test)]
            #[path = "fixtures/helpers.rs"]
            mod helpers;
            #[cfg(test)]
            mod tests {
                #[test]
                fn t() { std::thread::spawn(|| {}); let _ = "{"; }
            }
            #[cfg(all(test, unix))]
            fn only_in_unix_tests() { std::thread::spawn(|| {}); }
            #[test]
            fn top_level_test() { std::thread::spawn(|| {}); }
            #[tokio::test]
            async fn async_test() { std::thread::spawn(|| {}); }
            #[cfg(not(test))]
            pub fn shipped() { std::thread::spawn(|| {}); }
            pub struct State {
                #[cfg(test)]
                probes: std::collections::HashMap<u8, u8>,
                #[cfg(test)]
                last: Option<u8>
            }
            pub fn after_fields() { std::thread::spawn(|| {}); }
            pub fn statement() {
                #[cfg(test)]
                if true { std::thread::spawn(|| {}); }
                std::thread::spawn(|| {});
            }
            #[cfg(unix)]
            #[test]
            fn unix_test() {}
            """,
        )
        self.write("apps/calendar/src/store.rs", "pub fn open() { std::thread::spawn(|| {}); }\n")
        for relative in (
            "src/store_tests.rs",
            "src/fixtures/helpers.rs",
            "src/nested/tests.rs",
            "tests/it.rs",
            "benches/b.rs",
            "examples/e.rs",
            "build.rs",
        ):
            self.write(f"apps/calendar/{relative}", "fn x() { std::thread::spawn(|| {}); }\n")
        self.write(
            "apps/calendar/src/gated.rs", "#![cfg(test)]\nfn x() { std::thread::spawn(|| {}); }\n"
        )
        sites = sorted((Path(f.path).name, f.line) for f in self.findings() if f.rule == "W1")
        self.assertEqual(sites, [("lib.rs", 19), ("lib.rs", 26), ("lib.rs", 30), ("store.rs", 1)])
        self.assertEqual([f for f in self.findings() if f.rule == "R1"], [])
        self.assertTrue(app.is_dir())

    def test_one_site_per_line_and_rule(self):
        self.app("feeds", 'pub fn a() { let _ = (std::fs::read("a"), std::fs::read("b")); }\n')
        self.assertEqual(self.rules("feeds"), {"W3": 1})

    def test_narrowed_platforms_skip_rules_that_do_not_apply(self):
        source = """\
        pub fn a() { std::thread::spawn(|| {}); }
        pub fn b() { std::process::Command::new("udisksctl"); }
        pub fn c() -> &'static str { "/run/udev" }
        #[cfg(unix)]
        pub fn d() {}
        """
        self.app("portable", source)
        self.app(
            "desktop",
            source,
            package_keys='platforms = ["linux", "macos"]\nplatform-reason = "PTY."\n',
        )
        self.app(
            "disks",
            source,
            package_keys='platforms = ["tilo-os"]\nplatform-reason = "udisks."\n',
        )
        self.assertEqual(set(self.rules("portable")), {"R1", "R3", "R4", "W1"})
        self.assertEqual(set(self.rules("desktop")), {"R1", "R3", "R4"})
        self.assertEqual(set(self.rules("disks")), {"R1"})
        # Platform code in an app on every platform has no baseline allowance
        # once `[strict]` names the rules.
        enforced = self.write(
            "strict_rules.toml",
            RULES.read_text(encoding="utf-8").replace(
                "rules = []", 'rules = ["R1", "R2", "R3", "R4", "W1", "W2", "W3"]', 1
            ),
        )
        strict = {(f.package, f.rule) for f in portability.audit(self.root, enforced) if f.strict}
        self.assertEqual(
            strict, {("portable", "R1"), ("portable", "R3"), ("portable", "R4"), ("portable", "W1")}
        )

    def test_a_target_section_holding_only_the_workspace_hack_is_not_platform_code(self):
        self.app(
            "plumbing",
            cargo_extra='\n[target.\'cfg(not(target_arch = "wasm32"))\'.dependencies]\n'
            'workspace-hack = { version = "0.1" }\n',
        )
        self.app(
            "selecting",
            cargo_extra='\n[target.\'cfg(not(target_arch = "wasm32"))\'.dependencies]\n'
            'workspace-hack = { version = "0.1" }\nlibc = "0.2"\n',
        )
        self.assertEqual(self.rules("plumbing"), {})
        self.assertEqual(self.rules("selecting").get("R1"), 1)

    def test_model_crates_get_only_the_model_rules(self):
        self.app(
            "notes-model",
            'pub fn a() { std::thread::spawn(|| {}); }\n#[cfg(unix)]\npub fn b() {}\n',
            dependencies="",
            manifest=False,
        )
        self.app("scratch", manifest=False)
        self.assertEqual(self.rules("notes-model"), {"R1": 1})
        self.assertEqual(self.findings("scratch"), [])

    def test_sdk_crates_count_target_cfg_and_web_traps_outside_the_runner(self):
        self.write(
            "crates/sdk/ui-core/Cargo.toml",
            '[package]\nname = "tilo-ui"\n\n'
            "[target.'cfg(target_os = \"linux\")'.dependencies]\nlibc = \"0.2\"\n",
        )
        self.write(
            "crates/sdk/ui-core/src/lib.rs",
            "#[cfg(windows)]\nfn a() {}\nfn b() { let _ = std::time::Instant::now(); }\n",
        )
        self.write(
            "crates/sdk/tilo-app/Cargo.toml",
            '[package]\nname = "tilo-app"\n\n'
            "[target.'cfg(target_arch = \"wasm32\")'.dependencies]\nweb = \"1\"\n",
        )
        self.write("crates/sdk/tilo-app/src/lib.rs", "mod runner;\npub fn run() {}\n")
        self.write("crates/sdk/tilo-app/src/runner.rs", "#[cfg(unix)]\nfn native() {}\n")
        found = self.findings()
        self.assertEqual(
            sorted((f.section, f.package, f.rule, Path(f.path).name) for f in found),
            [
                ("sdk", "tilo-ui", "R1", "Cargo.toml"),
                ("sdk", "tilo-ui", "R1", "lib.rs"),
                ("sdk", "tilo-ui", "W2", "lib.rs"),
            ],
        )


class BaselineTests(unittest.TestCase):
    def counts(self, apps=None, sdk=None):
        return {"apps": apps or {}, "sdk": sdk or {}}

    def test_equal_counts_pass(self):
        current = self.counts({"notes": {"R1": 2}}, {"tilo-ui": {"W3": 1}})
        self.assertEqual(portability.compare(current, current), ([], []))

    def test_growth_and_new_packages_fail(self):
        baseline = self.counts({"notes": {"R1": 2}})
        current = self.counts({"notes": {"R1": 3, "W1": 1}, "chat": {"D1": 1}})
        regressions, stale = portability.compare(current, baseline)
        self.assertEqual(stale, [])
        self.assertEqual(len(regressions), 3)
        self.assertTrue(any("must be clean" in line and "chat" in line for line in regressions))
        self.assertTrue(any("baseline allows 2" in line for line in regressions))

    def test_falling_counts_are_stale_and_the_rendered_baseline_drops_zeroes(self):
        baseline = self.counts({"notes": {"R1": 2, "W1": 1}, "gone": {"D1": 1}})
        current = self.counts({"notes": {"R1": 1}})
        regressions, stale = portability.compare(current, baseline)
        self.assertEqual(regressions, [])
        self.assertEqual(len(stale), 3)
        rendered = portability.render_baseline(current)
        self.assertIn("[apps.notes]\nR1 = 1\n", rendered)
        self.assertNotIn("gone", rendered)

    def test_render_and_load_round_trip(self):
        current = self.counts({"notes": {"R1": 2, "W2": 5}}, {"tilo-ui": {"R1": 7}})
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "baseline.toml"
            path.write_text(portability.render_baseline(current), encoding="utf-8")
            self.assertEqual(portability.load_baseline(path), current)


class LexerTests(unittest.TestCase):
    def test_literals_comments_and_lifetimes(self):
        code, literals = portability.lex(
            "fn f<'a>(x: &'a str) -> char { /* \"no\" /* nested */ */ let _ = b\"b\"; '\"' }\n"
            'const R: &str = r##"a "# b"##; // "tail"\n'
        )
        self.assertEqual([literal.value for literal in literals], ["b", 'a "# b'])
        self.assertTrue(literals[0].byte)
        self.assertIn("fn f<'a>(x: &'a str)", code)
        self.assertNotIn("nested", code)
        self.assertNotIn("tail", code)
        self.assertEqual(code.count("\n"), 2)

    def test_requires_test(self):
        self.assertTrue(portability.requires_test("test"))
        self.assertTrue(portability.requires_test("all(test, unix)"))
        self.assertTrue(portability.requires_test("any(test, all(test, windows))"))
        self.assertFalse(portability.requires_test("not(test)"))
        self.assertFalse(portability.requires_test("any(test, unix)"))


if __name__ == "__main__":
    unittest.main()
