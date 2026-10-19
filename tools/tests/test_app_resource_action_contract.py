import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "policy/check_app_resource_action_contract.py"
SPEC = importlib.util.spec_from_file_location("check_app_resource_action_contract", SCRIPT)
contract = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = contract
SPEC.loader.exec_module(contract)


class AppResourceActionContractTests(unittest.TestCase):
    def test_code_runs_preserve_delimiters_lifetimes_and_unicode(self):
        cases = [
            ("let ratio = α / β;\n", "let ratio = α / β;\n"),
            ("fn f<'a>(x: &'a str) {}", "fn f<'a>(x: &'a str) {}"),
            ("a// hidden\nb", "a         \nb"),
            ("a/* hidden */b", "a            b"),
            ('a"hidden\\\"text"b', "a              b"),
            ("a('/', '\"')b", "a(   ,    )b"),
        ]
        for source, expected in cases:
            with self.subTest(source=source):
                self.assertEqual(contract.code_only(source), expected)

    def fixture(
        self,
        source: str,
        *,
        dependencies: str = 'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n',
        app_name: str = "sample",
    ) -> Path:
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        app = root / "apps" / app_name
        (app / "src").mkdir(parents=True)
        (app / "Cargo.toml").write_text(
            f'[package]\nname="{app_name}"\nversion="0.1.0"\n[dependencies]\n'
            + dependencies,
            encoding="utf-8",
        )
        (app / "src" / "lib.rs").write_text(source, encoding="utf-8")
        return root

    def test_char_literals_do_not_hide_the_code_after_them(self):
        code = contract.code_only(
            "fn f<'a>(s: &'a str) -> String { s.replace('\"', \"\").trim_matches('\\'') }\n"
            "fn handler() { start_operation(); }\n"
        )
        self.assertIn("fn handler() { start_operation(); }", code)
        self.assertIn("fn f<'a>(s: &'a str)", code)
        self.assertNotIn('"', code)

    def test_repeated_audits_observe_source_changes_at_the_same_path(self):
        valid = (
            "pub struct Note;\n"
            "impl Resource for Note { fn register(registry: &mut ActionRegistry) {} }\n"
            "struct Sample;\n"
            "impl ResourceApp for Sample { type Resource = Note; }\n"
        )
        root = self.fixture(valid)
        source = root / "apps/sample/src/lib.rs"
        (source.parent / "main.rs").write_text("ui::main!(sample::Sample);\n")
        self.assertEqual(contract.audit(root), ([], 1))
        source.write_text("struct Sample;\nimpl ResourceApp for Sample {}\n")
        violations, apps = contract.audit(root)
        self.assertEqual(apps, 1)
        self.assertIn("Widget::resources", "\n".join(item.reason for item in violations))
        source.write_text(valid)
        self.assertEqual(contract.audit(root), ([], 1))

    def test_resource_surface_requires_a_real_nonempty_resource_provider(self):
        with self.subTest(phase='resource action app passes'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
                " fn local_resource_operation_handlers(&self) -> &'static [&'static str] { &[\"app.open\"] }\n"
                " fn start_operation(&mut self, type_id: &str) { match type_id { \"app.open\" => open(), _ => {} } } }\n"
            )
            violations, apps = contract.audit(root)
            self.assertEqual(violations, [])
            self.assertEqual(apps, 1)
        with self.subTest(phase='framework app with a resource passes'):
            root = self.fixture(
                "#[derive(Model, Default)] pub struct Note { pub body: String }\n"
                "#[resource(kind = \"sample.note\", label = \"Note\")]\n"
                "impl Note { #[action] pub fn edit(&mut self, text: String) { self.body = text; } }\n"
                "struct Sample;\n"
                "impl ResourceApp for Sample { type Resource = Note; }\n"
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='a framework app whose tests check published effects passes'):
            root = self.fixture(
                "pub struct Vault;\n"
                "impl Resource for Vault { fn register(registry: &mut ActionRegistry) {} }\n"
                "struct Sample;\n"
                "impl ResourceApp for Sample { type Resource = Vault; }\n"
                "#[cfg(test)]\nmod tests {\n"
                " fn copy_is_an_effect(spec: ActionSpec) { assert_eq!(spec.kind, OperationKind::Effect); }\n"
                "}\n"
            )
            (root / "apps" / "sample" / "src" / "main.rs").write_text(
                "ui::main!(sample::Sample);\n", encoding="utf-8"
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='framework app without a resource type fails'):
            root = self.fixture("struct Sample;\nimpl ResourceApp for Sample {}\n")
            violations, _ = contract.audit(root)
            self.assertIn("Widget::resources", "\n".join(item.reason for item in violations))
        with self.subTest(phase='missing protocol and surface fail'):
            root = self.fixture("struct App;\n", dependencies="")
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertIn("tilo-app", reasons)
            self.assertIn("Widget::resources", reasons)
            self.assertIn("ActionSpec", reasons)
        with self.subTest(phase='comments and empty provider do not satisfy contract'):
            root = self.fixture(
                "// ActionSpec::operation Resource::new\n"
                "impl Widget for App { fn resources(&self) -> Vec<Resource> { vec![] } }\n"
            )
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertIn("ActionSpec", reasons)
            self.assertIn("empty provider", reasons)
        with self.subTest(phase='toolkit resource action host needs no copied widget boilerplate'):
            root = self.fixture(
                "pub fn app_root() {\n"
                " let resource = ui::widget::resource_actions::typed_resource();\n"
                " ui::widget::resource_actions::immediate(root, resource, |_| {});\n"
                "}\n"
            )
            violations, apps = contract.audit(root)
            self.assertEqual(violations, [])
            self.assertEqual(apps, 1)
        with self.subTest(phase='toolkit host with a hand-built multi-operation descriptor passes'):
            root = self.fixture(
                "pub fn app_root() {\n"
                " let mut resource = tilo_actions::Resource::new(\"k:root\", \"k\", \"K\");\n"
                " resource.actions.push(ActionSpec::operation(\"a\", \"A\", OperationKind::Effect, T, &()).unwrap());\n"
                " ui::widget::resource_actions::immediate(root, resource, |_| {});\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='headless service publishing to the broker passes'):
            source = "fn publish() -> CollabRequest { CollabRequest::PublishActions { app_id, actions } }\n"
            service = (
                '[package]\nkind = "service"\n[entrypoint]\nbin = "bin/svc"\ntype = "cli"\n'
            )
            root = self.fixture(source)
            (root / "apps" / "sample" / "tilo-app.toml").write_text(service, encoding="utf-8")
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='a windowed app publishing to the broker still needs resources'):
            root = self.fixture(source)
            (root / "apps" / "sample" / "tilo-app.toml").write_text(
                service.replace('type = "cli"', 'type = "gui"'), encoding="utf-8"
            )
            violations, _ = contract.audit(root)
            self.assertIn("Widget::resources", "\n".join(item.reason for item in violations))
        with self.subTest(phase='a headless service publishing nothing still needs resources'):
            root = self.fixture("struct Service;\n")
            (root / "apps" / "sample" / "tilo-app.toml").write_text(service, encoding="utf-8")
            violations, _ = contract.audit(root)
            self.assertIn("Widget::resources", "\n".join(item.reason for item in violations))
        with self.subTest(phase='toolkit host without resource descriptor fails'):
            root = self.fixture(
                "pub fn app_root() {\n"
                " ui::widget::resource_actions::immediate(root, resource, |_| {});\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("no toolkit resource descriptor" in item.reason for item in violations))

    def test_a_shared_framework_app_definition_publishes_its_resource(self):
        def delegated(alias: str = "shared_ui", item: str = "SampleApp") -> str:
            return (
                f"pub use {alias}::{item};\n"
                "pub fn definition() -> &'static ui::application::ApplicationDefinition {\n"
                f"    {alias}::definition()\n"
                "}\n"
            )

        def shared(definition: str = "ui::built_in_definition!(SampleApp)") -> str:
            return (
                "#[derive(Model, Default)] pub struct Note { pub body: String }\n"
                '#[resource(kind = "shared.note", label = "Note")]\n'
                "impl Note {}\n"
                "pub struct SampleApp;\n"
                "impl ResourceApp for SampleApp { type Resource = Note; }\n"
                "pub fn definition() -> &'static ApplicationDefinition {\n"
                f"    {definition}\n"
                "}\n"
            )

        def fixture(
            *,
            source: str = delegated(),
            dependencies: str = (
                'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n'
                'shared_ui={path="../../crates/shared-ui"}\n'
            ),
            code: str = shared(),
            shared_name: str = "shared-ui",
            extra: dict[str, str] | None = None,
        ) -> Path:
            root = self.fixture(source, dependencies=dependencies)
            crate = root / "crates" / shared_name / "src"
            crate.mkdir(parents=True)
            (crate / "lib.rs").write_text(code, encoding="utf-8")
            for relative, text in (extra or {}).items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            return root

        def reasons(root: Path) -> str:
            violations, _ = contract.audit(root)
            return "\n".join(item.reason for item in violations)

        with self.subTest(phase="delegated definition passes"):
            violations, _ = contract.audit(fixture())
            self.assertEqual(violations, [])
        with self.subTest(phase="a renamed normal dependency keeps its Rust alias"):
            renamed = (
                'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n'
                'renamed={package="shared-ui",path="../../crates/shared-ui"}\n'
            )
            violations, _ = contract.audit(
                fixture(source=delegated("renamed"), dependencies=renamed)
            )
            self.assertEqual(violations, [])
        with self.subTest(phase="re-export without delegating definition still fails"):
            self.assertIn("Widget::resources", reasons(fixture(source="pub use shared_ui::SampleApp;\n")))
        with self.subTest(phase="delegation to a crate without a resource still fails"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "pub struct SampleApp;\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="another ResourceApp does not stand in for the host"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "#[derive(Model, Default)] pub struct Note {}\n"
                            '#[resource(kind = "shared.note", label = "Note")]\n'
                            "impl Note {}\n"
                            "pub struct SampleApp;\n"
                            "pub struct Other;\n"
                            "impl ResourceApp for Other { type Resource = Note; }\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="the host's resource must be the declared one"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "#[derive(Model, Default)] pub struct Note {}\n"
                            '#[resource(kind = "shared.note", label = "Note")]\n'
                            "impl Note {}\n"
                            "pub struct Missing;\n"
                            "pub struct SampleApp;\n"
                            "impl ResourceApp for SampleApp { type Resource = Missing; }\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="a cfg(test) host publishes nothing"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "pub struct SampleApp;\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                            "#[cfg(test)]\n"
                            "mod tests {\n"
                            "    #[derive(Model, Default)] pub struct Note {}\n"
                            '    #[resource(kind = "shared.note", label = "Note")]\n'
                            "    impl Note {}\n"
                            "    impl ResourceApp for SampleApp { type Resource = Note; }\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="a _tests.rs host publishes nothing"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "pub struct SampleApp;\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                        ),
                        extra={
                            "crates/shared-ui/src/app_tests.rs": (
                                "pub struct SampleApp;\n"
                                "impl ResourceApp for SampleApp { type Resource = Note; }\n"
                                "#[resource(kind = \"shared.note\", label = \"Note\")]\n"
                                "impl Note {}\n"
                            )
                        },
                    )
                ),
            )
        with self.subTest(phase="a cfg(test) module file publishes nothing"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "pub struct SampleApp;\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    ui::built_in_definition!(SampleApp)\n"
                            "}\n"
                            "#[cfg(test)]\n"
                            "mod support;\n"
                        ),
                        extra={
                            "crates/shared-ui/src/support.rs": (
                                "pub struct SampleApp;\n"
                                "impl ResourceApp for SampleApp { type Resource = Note; }\n"
                                "#[resource(kind = \"shared.note\", label = \"Note\")]\n"
                                "impl Note {}\n"
                            )
                        },
                    )
                ),
            )
        for table in ("dev-dependencies", "build-dependencies"):
            with self.subTest(phase=f"a {table} host publishes nothing"):
                self.assertIn(
                    "Widget::resources",
                    reasons(
                        fixture(
                            dependencies=(
                                'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n'
                                f"[{table}]\n"
                                'shared_ui={path="../../crates/shared-ui"}\n'
                            )
                        )
                    ),
                )
        with self.subTest(phase="a todo!() definition publishes nothing"):
            self.assertIn(
                "Widget::resources",
                reasons(fixture(code=shared("todo!()"))),
            )
        with self.subTest(phase="an unused construction does not substitute for the returned definition"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "#[derive(Model, Default)] pub struct Note {}\n"
                            '#[resource(kind = "shared.note", label = "Note")]\n'
                            "impl Note {}\n"
                            "pub struct SampleApp;\n"
                            "impl ResourceApp for SampleApp { type Resource = Note; }\n"
                            "fn actual_definition() -> &'static ApplicationDefinition {\n"
                            "    todo!()\n"
                            "}\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    let _ = ui::built_in_definition!(SampleApp);\n"
                            "    actual_definition()\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="a private namesake does not stand in for the exported definition"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code=(
                            "#[derive(Model, Default)] pub struct Note {}\n"
                            '#[resource(kind = "shared.note", label = "Note")]\n'
                            "impl Note {}\n"
                            "pub struct SampleApp;\n"
                            "impl ResourceApp for SampleApp { type Resource = Note; }\n"
                            "pub struct Other;\n"
                            "impl ResourceApp for Other { type Resource = Missing; }\n"
                            "fn actual_definition() -> &'static ApplicationDefinition {\n"
                            "    ui::framework::definition::<Other>()\n"
                            "}\n"
                            "pub fn definition() -> &'static ApplicationDefinition {\n"
                            "    actual_definition()\n"
                            "}\n"
                            "mod other {\n"
                            "    pub fn definition() -> &'static ApplicationDefinition {\n"
                            "        ui::built_in_definition!(SampleApp)\n"
                            "    }\n"
                            "}\n"
                        )
                    )
                ),
            )
        with self.subTest(phase="a definition re-exported from a cfg(test) module publishes nothing"):
            self.assertIn(
                "Widget::resources",
                reasons(
                    fixture(
                        code="#[cfg(test)]\nmod app;\npub use app::definition;\n",
                        extra={"crates/shared-ui/src/app.rs": shared()},
                    )
                ),
            )
        with self.subTest(phase="re-exporting an unrelated item does not name the host"):
            self.assertIn(
                "Widget::resources",
                reasons(fixture(source=delegated(item="Note"))),
            )

    def test_only_a_compute_package_publishes_no_surface_of_its_own(self):
        compute_manifest = (
            '[package]\nid="so.tilo.sample"\nkind="service"\n'
            '[entrypoint]\nbin="bin/sample-batch"\ntype="cli"\n'
            '[cli]\ncommands=["sample-batch"]\n'
        )
        with self.subTest(phase='a cli service with commands needs no facade or resources'):
            root = self.fixture("fn main() {}\n", dependencies="")
            (root / "apps" / "sample" / "tilo-app.toml").write_text(
                compute_manifest, encoding="utf-8"
            )
            violations, apps = contract.audit(root)
            self.assertEqual(violations, [])
            self.assertEqual(apps, 1)
        with self.subTest(phase='a compute package still may not bypass the facade'):
            root = self.fixture(
                "fn main() {}\n",
                dependencies='ui_core={package="ui",path="../../crates/sdk/ui-core"}\n',
            )
            (root / "apps" / "sample" / "tilo-app.toml").write_text(
                compute_manifest, encoding="utf-8"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))
        for phase, manifest in (
            ("a cli service without commands", compute_manifest.replace('"sample-batch"', "")),
            ("a gui app with commands", compute_manifest.replace('type="cli"', 'type="gui"')),
            ("a cli app that is not a service", compute_manifest.replace('"service"', '"app"')),
        ):
            with self.subTest(phase=f"{phase} must publish its own surface"):
                root = self.fixture("fn main() {}\n", dependencies="")
                (root / "apps" / "sample" / "tilo-app.toml").write_text(manifest, encoding="utf-8")
                violations, _ = contract.audit(root)
                reasons = "\n".join(item.reason for item in violations)
                self.assertIn("tilo-app", reasons)
                self.assertIn("Widget::resources", reasons)

    def test_search_providers_must_be_budgeted_paged_and_catalogue_stable(self):
        with self.subTest(phase='app search provider must override limited and paged queries'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search(&self, _: &str) -> Vec<SearchResult> { vec![] }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertIn("unbounded search_limited fallback", reasons)
            self.assertIn("non-paged search_page fallback", reasons)
        with self.subTest(phase='app search provider with budgeted override passes search gate'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search(&self, q: &str) -> Vec<SearchResult> { self.search_limited(q, 32) }\n"
                " fn search_limited(&self, _: &str, limit: usize) -> Vec<SearchResult> {\n"
                "   self.rows.iter().take(limit).cloned().collect()\n"
                " }\n"
                " fn search_page(&self, q: &str, cursor: Option<&SearchCursor>, work: usize, limit: usize) -> SearchPage {\n"
                "   self.catalog.query_page(q, cursor, work, limit)\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertNotIn("search_limited", reasons)
            self.assertNotIn("search_page", reasons)
        with self.subTest(phase='limited only provider still fails paging gate'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search_limited(&self, _: &str, limit: usize) -> Vec<SearchResult> {\n"
                "   self.rows.iter().take(limit).cloned().collect()\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("non-paged search_page fallback" in item.reason for item in violations))
        with self.subTest(phase='paged provider cannot materialise a legacy search then slice it'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search_limited(&self, _: &str, _: usize) -> Vec<SearchResult> { vec![] }\n"
                " fn search_page(&self, q: &str, _: Option<&SearchCursor>, _: usize, limit: usize) -> SearchPage {\n"
                "   let rows = self.search_limited(q, limit); SearchPage::new(rows)\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("materialising search fallback" in item.reason for item in violations))
        with self.subTest(phase='limited provider cannot search everything then truncate'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search(&self, _: &str) -> Vec<SearchResult> { vec![] }\n"
                " fn search_limited(&self, q: &str, limit: usize) -> Vec<SearchResult> {\n"
                "   let mut rows = self.search(q); rows.truncate(limit); rows\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("calls unbounded search" in item.reason for item in violations))
        with self.subTest(phase='catalog backed provider passes search gate'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl ui::search::CatalogSearchProvider for Corpus {\n"
                " fn provider_name(&self) -> &str { \"Corpus\" }\n"
                " fn catalog(&self) -> Option<SearchCatalog> { Some(self.catalog.clone()) }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertNotIn("search_limited", reasons)
            self.assertNotIn("search_page", reasons)
            self.assertNotIn("catalogue per query", reasons)
        with self.subTest(phase='catalog backed provider cannot rebuild its catalogue per query'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl CatalogSearchProvider for Corpus {\n"
                " fn provider_name(&self) -> &str { \"Corpus\" }\n"
                " fn catalog(&self) -> Option<SearchCatalog> {\n"
                "   let catalog = SearchCatalog::new(\"Corpus\");\n"
                "   let _ = catalog.replace_all(self.rows());\n"
                "   Some(catalog)\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("catalogue per query" in item.reason for item in violations))
        with self.subTest(phase='durable provider cannot truncate index before query'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "impl SearchProvider for Corpus {\n"
                " fn name(&self) -> &str { \"Corpus\" }\n"
                " fn search(&self, q: &str) -> Vec<SearchResult> { self.search_limited(q, 32) }\n"
                " fn search_limited(&self, _: &str, limit: usize) -> Vec<SearchResult> {\n"
                "   self.rows.iter().take(DEFAULT_SEARCH_INDEX_LIMIT).take(limit).cloned().collect()\n"
                " }\n"
                "}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("truncates its corpus" in item.reason for item in violations))

    def test_effect_actions_need_a_reachable_handler_registry(self):
        with self.subTest(phase='effect action without handler registry fails'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
                " fn start_operation(&mut self) {} }\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("handler registry" in item.reason for item in violations))
        with self.subTest(phase='legacy room entry point fails'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\nfn apply_room_action(&mut self) {}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("legacy room-action" in item.reason for item in violations))
        with self.subTest(phase='self attested handler registry fails'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::operation(\"open\", \"Open\", OperationKind::View, \"app.open\", &()).unwrap())] }\n"
                " fn local_resource_operation_handlers(&self) -> &'static [&'static str] { &[\"app.open\"] }\n"
                " fn start_operation(&mut self) { Ok(None) } }\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("not reachable" in item.reason for item in violations))
        with self.subTest(phase='canonical handler reaches registered helper'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "const APP_OPEN: &str = \"app.open\";\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::operation(\"open\", \"Open\", OperationKind::Effect, \"app.open\", &()).unwrap())] }\n"
                " fn local_resource_operation_handlers(&self) -> &'static [&'static str] { &[APP_OPEN] }\n"
                " fn start_operation(&mut self) { self.start_local_operation(); } }\n"
                "fn start_local_operation(&mut self) { let _ = APP_OPEN; }\n"
            )
            violations, _ = contract.audit(root)
            self.assertFalse(any("not reachable" in item.reason for item in violations))
        with self.subTest(phase='framework surface answering typed effects passes'):
            root = self.fixture(
                "#[derive(Model, Default)] pub struct Note { pub body: String }\n"
                "#[resource(kind = \"sample.note\", label = \"Note\")]\n"
                "impl Note { #[action] pub fn edit(&mut self, text: String) { self.body = text; } }\n"
                "struct Sample;\n"
                "impl ResourceApp for Sample { type Resource = Note; }\n"
                "impl ResourceSurface<Note, Show> for View {\n"
                " fn effect(&mut self, effect: Show, cx: &mut EffectCx<Note, Self>) -> EffectAnswer { todo() } }\n"
                "fn check(spec: &ActionSpec) -> bool { spec.kind == OperationKind::View }\n"
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])

    def test_direct_ui_core_dependencies_fail_in_every_form(self):
        with self.subTest(phase='direct ui core dependency fails even when renamed'):
            root = self.fixture(
                "fn resources() {}\nstruct ActionDescriptor;\n"
                "RemoteActionMessage::CatalogueRequest; RemoteActionMessage::Invoke(request);\n"
                "RemoteActionMessage::Observe;\n",
                dependencies=(
                    'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n'
                    'ui_core={package="ui",path="../../crates/sdk/ui-core"}\n'
                ),
                app_name="agent",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))
        with self.subTest(phase='direct ui core path fails without explicit package name'):
            root = self.fixture(
                "fn resources() {}\nstruct ActionDescriptor;\n"
                "RemoteActionMessage::CatalogueRequest; RemoteActionMessage::Invoke(request);\n"
                "RemoteActionMessage::Observe;\n",
                dependencies=(
                    'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n'
                    'rendering={path="../../crates/sdk/ui-core"}\n'
                ),
                app_name="agent",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))
        with self.subTest(phase='target specific facades are checked'):
            root = self.fixture(
                "fn resources() {}\nstruct ActionDescriptor;\n"
                "RemoteActionMessage::CatalogueRequest; RemoteActionMessage::Invoke(request);\n"
                "RemoteActionMessage::Observe;\n",
                dependencies="",
                app_name="agent",
            )
            manifest = root / "apps" / "agent" / "Cargo.toml"
            manifest.write_text(
                manifest.read_text(encoding="utf-8")
                + "\n[target.'cfg(unix)'.dependencies]\n"
                + 'ui={package="tilo-app",path="../../crates/sdk/tilo-app"}\n',
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])

            manifest.write_text(
                manifest.read_text(encoding="utf-8")
                + "\n[target.'cfg(windows)'.dependencies]\n"
                + 'ui_core={package="ui",path="../../crates/sdk/ui-core"}\n',
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("direct ui-core dependency" in item.reason for item in violations))

    def test_raw_callback_ratchet_is_zero_by_default_and_reduce_only(self):
        with self.subTest(phase='new raw callback exceeds zero default ratchet'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "fn view() { Button::new().on_click(|| mutate_domain()); }\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("raw on_click closure" in item.reason for item in violations))
        with self.subTest(phase='audited callback ceiling is reduce only'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "fn view() { Button::new().on_click(|| mutate_domain()); }\n"
            )
            baseline = root / contract.BASELINE / "sample.toml"
            baseline.parent.mkdir(parents=True)
            baseline.write_text(
                '[ratchets.raw_on_click_closure]\n"apps/sample/src/lib.rs"=1\n',
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])

            app = root / "apps" / "sample" / "src" / "lib.rs"
            app.write_text(
                app.read_text(encoding="utf-8")
                + "fn second() { Button::new().on_click(move || mutate_more()); }\n",
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("audited ceiling 1" in item.reason for item in violations))
        with self.subTest(phase='reducing debt requires lowering the audited ceiling'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            )
            baseline = root / contract.BASELINE / "sample.toml"
            baseline.parent.mkdir(parents=True)
            baseline.write_text(
                '[ratchets.raw_on_click_closure]\n"apps/sample/src/lib.rs"=1\n',
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("stale raw_on_click_closure ceiling" in item.reason for item in violations))
        with self.subTest(phase="each ceiling lives in its app's baseline file"):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "fn view() { Button::new().on_click(|| mutate_domain()); }\n"
            )
            baseline = root / contract.BASELINE / "other.toml"
            baseline.parent.mkdir(parents=True)
            baseline.write_text(
                '[ratchets.raw_on_click_closure]\n"apps/sample/src/lib.rs"=1\n',
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("belongs in" in item.reason for item in violations))

    def test_bespoke_automation_palette_and_search_activation_paths_fail(self):
        with self.subTest(phase='new bespoke automation entry point fails'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "fn resources(&self) {}\n"
                "fn automation_invoke() {}\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("bespoke automation" in item.reason for item in violations))
        with self.subTest(phase='private command palette paths fail'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "fn private(palette: &mut Palette) -> Option<CommandOutcome> {\n"
                " palette.open_argument(\"go\", \"Go\", \"Value\", None); None }\n"
            )
            violations, _ = contract.audit(root)
            reasons = "\n".join(item.reason for item in violations)
            self.assertIn("private command-palette argument route", reasons)
            self.assertIn("private command-palette outcome", reasons)
        with self.subTest(phase='new opaque search activation is rejected'):
            root = self.fixture(
                "use tilo_actions::ActionSpec;\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] }\n"
                " fn activate_search_result(&mut self, id: &str) { private_open(id); } }\n"
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("opaque search activation bypass" in item.reason for item in violations))

    def test_only_live_production_registration_satisfies_the_contract(self):
        with self.subTest(phase='test only action registration does not satisfy live contract'):
            root = self.fixture(
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "#[test] fn registration_test() { actions::register(&mut registry()); }\n"
            )
            main = root / "apps" / "sample" / "src" / "main.rs"
            main.write_text("fn main() { launch(App); }\n", encoding="utf-8")
            violations, _ = contract.audit(root)
            self.assertTrue(any("production app launch" in item.reason for item in violations))
        with self.subTest(phase='dead main file registration helper does not satisfy live contract'):
            root = self.fixture(
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            )
            main = root / "apps" / "sample" / "src" / "main.rs"
            main.write_text(
                "fn main() { launch(App); }\n"
                "#[cfg(test)] fn configure_for_test(app: &mut App) {\n"
                " actions::register(app.action_registry_mut()); }\n",
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("production app launch" in item.reason for item in violations))
        with self.subTest(phase='production action registration satisfies live contract'):
            root = self.fixture(
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            )
            main = root / "apps" / "sample" / "src" / "main.rs"
            main.write_text(
                "fn main() { LaunchSpec::new(root).configure_app(|app| "
                "actions::register(app.action_registry_mut())); }\n",
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertEqual(violations, [])
        with self.subTest(phase='prepared launch installs the registered registry'):
            source = (
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
            )
            launch = "fn main() { LaunchSpec::new(root).prepare_legacy_with_args(prepare); }\n"
            inputs = (
                "fn inputs(args: &Args) -> Result<LegacyLaunchInputs, Error> { {\n"
                " let mut registry = ActionRegistry::new();\n"
                " actions::register(&mut registry);\n"
                " Ok(ui::launch::LegacyLaunchInputs::new(state, widget, autosave)\n"
                " .with_records(records, room.doc).with_registry(registry),) } }\n"
            )
            wrapper = (
                "fn prepare(args: &Args) -> Result<LegacyLaunchInputs, Error> {\n"
                " inputs(args).map_err(|error| Error::Operation(error.to_string())) }\n"
            )
            cases = {
                "forwarded": (launch + wrapper + inputs, True),
                "direct": (launch + inputs.replace("fn inputs(", "fn prepare("), True),
                "widget_owned_persistence": (
                    launch + wrapper + inputs.replace(
                        ".with_records(records, room.doc).with_registry(registry)",
                        ".with_registry(registry).with_widget_owned_persistence()",
                    ), True,
                ),
                "widget_owned_overwritten_registry": (
                    launch + wrapper + inputs.replace(
                        ".with_registry(registry)",
                        ".with_registry(registry).with_widget_owned_persistence().with_registry(other)",
                    ), False,
                ),
                "widget_owned_missing_registry": (
                    launch + wrapper + inputs.replace(
                        ".with_registry(registry)", ".with_widget_owned_persistence()",
                    ), False,
                ),
                "disconnected": (
                    launch + wrapper + inputs.replace("with_registry(registry)", "with_registry(other)"),
                    False,
                ),
                "missing_installation": (
                    launch + wrapper + inputs.replace(".with_registry(registry)", ""), False,
                ),
                "shadowed_registry": (
                    launch + wrapper + inputs.replace(
                        " Ok(", " let registry = ActionRegistry::new(); Ok("
                    ), False,
                ),
                "overwritten_registry": (
                    launch + wrapper + inputs.replace(
                        ".with_registry(registry)", ".with_registry(registry).with_registry(other)"
                    ), False,
                ),
                "discarded_helper_result": (
                    launch + inputs + "fn prepare(args: &Args) -> Result<LegacyLaunchInputs, Error> {"
                    " inputs(args)?; Ok(LegacyLaunchInputs::new(state, widget, autosave)) }", False,
                ),
                "discarded_inputs": (
                    launch + wrapper + inputs.replace(
                        "),) } }", "),); Ok(LegacyLaunchInputs::new(state, widget, autosave)) } }"
                    ), False,
                ),
                "dead_preparer": ("fn main() { launch(root); }" + wrapper + inputs, False),
            }
            for name, (main_source, accepted) in cases.items():
                with self.subTest(name=name):
                    root = self.fixture(source)
                    (root / "apps/sample/src/main.rs").write_text(main_source, encoding="utf-8")
                    violations, _ = contract.audit(root)
                    if accepted:
                        self.assertEqual(violations, [])
                    else:
                        self.assertTrue(any("production app launch" in v.reason for v in violations))
        with self.subTest(phase='component factory installs the registered registry'):
            source = (
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "static DEFINITION: LazyLock<ApplicationDefinition> = LazyLock::new(|| "
                "ApplicationDefinition { create: |context, target, restore| "
                "create_component(context, target, restore) });\n"
                "fn create_component(context: HostContext, target: &LaunchTarget, "
                "restore: &RestoreInput) -> Result<AppComponent, Error> {\n"
                " let mut registry = ActionRegistry::new();\n"
                " actions::register(&mut registry);\n"
                " Ok(AppComponent::new(context, Box::new(App), registry))\n"
                "}\n"
            )
            root = self.fixture(source)
            main = root / "apps" / "sample" / "src" / "main.rs"
            main.write_text(
                "fn main() { run(&LaunchSpec::from_definition(definition())"
                ".component_host(prepare_component)); }\n",
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertFalse(any("production app launch" in item.reason for item in violations))
        with self.subTest(phase='disconnected component registry does not satisfy live contract'):
            source = (
                "use tilo_actions::{ActionRegistry, ActionSpec};\n"
                "pub fn register(_: &mut ActionRegistry) {}\n"
                "impl Widget for App { fn resources(&self, _: &Doc) -> Vec<Resource> {\n"
                " vec![Resource::new(\"app\", \"app\", \"App\").with_action(\n"
                " ActionSpec::immediate(\"go\", \"Go\", \"app.go\", &()).unwrap())] } }\n"
                "static DEFINITION: LazyLock<ApplicationDefinition> = LazyLock::new(|| "
                "ApplicationDefinition { create: |context, target, restore| "
                "create_component(context, target, restore) });\n"
                "fn create_component(context: HostContext, target: &LaunchTarget, "
                "restore: &RestoreInput) -> Result<AppComponent, Error> {\n"
                " Ok(AppComponent::new(context, Box::new(App), ActionRegistry::new()))\n"
                "}\n"
                "fn test_only(context: HostContext) -> AppComponent {\n"
                " let mut registry = ActionRegistry::new(); actions::register(&mut registry);\n"
                " AppComponent::new(context, Box::new(App), registry)\n"
                "}\n"
            )
            root = self.fixture(source)
            main = root / "apps" / "sample" / "src" / "main.rs"
            main.write_text(
                "fn main() { run(&LaunchSpec::from_definition(definition())"
                ".component_host(prepare_component)); }\n",
                encoding="utf-8",
            )
            violations, _ = contract.audit(root)
            self.assertTrue(any("production app launch" in item.reason for item in violations))


    def test_old_app_apis_are_refused_in_apps_and_examples(self):
        def banned(source: str, *, endpoint: bool = False) -> list[str]:
            root = Path(self.enterContext(tempfile.TemporaryDirectory()))
            path = root / "apps" / "notes" / "src" / "lib.rs"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(source, encoding="utf-8")
            violations: list = []
            contract.audit_legacy_apis([(path, source)], violations, endpoint=endpoint)
            return [item.reason for item in violations]

        def rules(source: str, *, endpoint: bool = False) -> dict[str, int]:
            found: dict[str, int] = {}
            for reason in banned(source, endpoint=endpoint):
                for rule, (_, text) in contract.LEGACY_API_BANS.items():
                    if reason == text:
                        found[rule] = found.get(rule, 0) + 1
            return found

        old = (
            "impl tilo_resource::Resource for Notes { }\n"
            "fn f(args: &ActionArgs, doc: &mut AutoCommit) {\n"
            " let a = ActionAddress::new(\"notes\", \"rename\");\n"
            " let d = ActionDecl::new(\"rename\", \"Rename\");\n"
            " let s = ActionSpec::immediate(\"go\");\n"
            " let t = args.text(\"title\");\n"
            " doc.put(automerge::ROOT, \"title\", t);\n"
            "}\n"
            "pub struct Draft { pub filter: tilo_resource::Json<Vec<String>> }\n"
            "tilo_resource::value_model!(Draft);\n"
        )
        with self.subTest(phase="each old API is refused, with no ceiling to raise"):
            self.assertEqual(
                rules(old),
                {rule: 1 for rule in contract.LEGACY_API_BANS},
                banned(old),
            )
        with self.subTest(phase="an endpoint's addresses are its data, nothing else is"):
            found = rules(old, endpoint=True)
            self.assertNotIn("action_address_new", found)
            self.assertEqual(len(found), len(contract.LEGACY_API_BANS) - 1, found)
        document_writes = [
            "fn f(doc: &mut AutoCommit) { doc.put(&obj, \"k\", 1); }\n",
            "fn f(state: &mut automerge::AutoCommit) { state.put(&obj, \"k\", 1); }\n",
            "fn f<T: Transactable>(target: &mut T) { target.put(&obj, \"k\", 1); }\n",
            "fn f(tx: &mut impl Transactable) { tx.put(&obj, \"k\", 1); }\n",
            "fn f() { let mut source = AutoCommit::new(); source.put(&obj, \"k\", 1); }\n",
            "fn f(d: &Doc) { let mut copy = d.clone();\n copy\n .put(automerge::ROOT, \"k\", 1); }\n",
            "fn f() { apply(|doc| doc.put(ROOT, \"k\", 1)); }\n",
            "fn f(doc: &mut AutoCommit) { doc.splice_text(&id, 0, 0, \"a\"); }\n",
            "fn f(doc: &mut Doc) { use automerge::ObjType; doc.put_object(&id, \"k\", ObjType::Map); }\n",
        ]
        for source in document_writes:
            with self.subTest(phase="a document write is refused", source=source):
                self.assertEqual(rules(source).get("direct_automerge_write"), 1, source)
        other_writes = [
            "use automerge::ROOT;\nfn f(content: &ContentStore) { content.put(&bytes); }\n",
            "use automerge::ROOT;\nfn f(seeds: &mut Seeds) { seeds.put(id, secret); }\n",
            "use automerge::ROOT;\nfn f(&self) { self.inner.content_store().put(PNG); }\n",
            "use automerge::ROOT;\nfn f(cache: &mut Lru) { cache.put(key, tile); }\n",
            "fn f(store: &mut Vault) { store.put(\"account\", secret); }\n",
            "fn f(dm: &mut tilo_doc::DocMut) { use automerge::ObjId; dm.splice_text(&id, 0, 0, \"a\"); }\n",
            "fn f(slot: Slot) { use automerge::ROOT; slot.put_scalar(&mut d, v); }\n",
        ]
        for source in other_writes:
            with self.subTest(phase="a store's put or a model edit is no document write", source=source):
                self.assertNotIn("direct_automerge_write", rules(source), source)
        action_args = [
            "fn f(args: &ActionArgs) { args.text(\"title\"); }\n",
            "fn f(args: &tilo_actions::ActionArgs) { args.get(name); }\n",
            "fn f(args: &serde_json::Value) { args.get(\"title\"); }\n",
            "fn f() { on(|args: &ActionArgs| args.get(\"title\")); }\n",
            "fn f() { on(|args| args.boolean(\"on\")); }\n",
        ]
        for source in action_args:
            with self.subTest(phase="untyped action arguments are refused", source=source):
                self.assertEqual(rules(source).get("untyped_action_args"), 1, source)
        command_lines = [
            "fn main() { let args: Vec<String> = std::env::args().collect(); args.get(1); }\n",
            "fn run(args: &[String]) { let mode = args.get(2); }\n",
            "fn call(args: &[Expr]) { let (a, b) = (args.first(), args.get(1)); }\n",
            "fn step(args: &BTreeMap<String, Value>) { args.get(param.name.as_ref()); }\n",
            "fn flags(args: &Args) { args.get(\"--out-dir\"); }\n",
            "fn saved(step: &Step) { let Kind::Action { args, .. } = &step.kind; args.get(\"title\"); }\n",
        ]
        for source in command_lines:
            with self.subTest(phase="a command line, a list or a step's bindings are no action arguments", source=source):
                self.assertNotIn("untyped_action_args", rules(source), source)
        with self.subTest(phase="the model's own APIs are not refused"):
            new = (
                "#[resource(kind = \"notes\")] impl Notes { #[action] fn rename(&mut self) {} }\n"
                "impl ResourceSurface<Notes, NotesView> for NotesApp {}\n"
                "fn f(seeds: &mut Seeds) { seeds.put(id, secret); }\n"
                "fn g() { let a = Notes::RENAME.on(\"notes\"); let b = resource.action_address(\"open\"); }\n"
                "#[derive(Model)] pub struct Account { pub config: serde_json::Value }\n"
                "fn h(input: PendingInput) { if let PendingInput::Json(text) = input {} }\n"
                "// a field that was `Json<T>` moves with `evolution::retype_json`\n"
            )
            self.assertEqual(banned(new), [])


if __name__ == "__main__":
    unittest.main()
