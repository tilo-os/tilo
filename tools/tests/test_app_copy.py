import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
for name in ("check_app_portability", "check_app_copy"):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"policy/{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
copy = sys.modules["check_app_copy"]


class AppCopyTests(unittest.TestCase):
    def keys(self, source):
        return {key for key, _ in copy.used_keys(source, {"settings", "net"})}

    def test_covers_direct_conditional_table_and_helper_keys(self):
        source = '''
        fn pane() {
            let key = if online { "net-online" } else { "net-offline" };
            caption(&t(key));
            t_args("new-pane-caption", &[("error", error.into())]);
            t(match mode {
                "net-stored-mode" => "net-mode-title",
                _ => "net-default-title",
            });
            let table = [("value", "settings-table-label")];
            failed("settings-refresh-error", error);
        }
        '''
        self.assertEqual(self.keys(source), {
            "net-online", "net-offline", "new-pane-caption", "net-mode-title",
            "net-default-title", "settings-table-label", "settings-refresh-error",
        })

    def test_machine_ids_are_not_translated(self):
        source = '''
        fn slug(&self) -> &str { "settings-pane-slug" }
        fn pane() {
            Button::new("settings-button-id", &t("settings-button-label"));
            dropdown.with_id("settings-picker-id");
            thread.name("settings-worker".into());
            request.thread_name("settings-portal");
            crate::device_read::thread("settings-reader", Box::new(job));
            DeviceInfo { icon: "net-device-icon".into() };
            self.push_confirm(body, package,
                ("settings-confirm-id", "settings-cancel-id"), action);
        }
        '''
        self.assertEqual(self.keys(source), {"settings-button-label"})

    def test_direct_resolver_keys_need_not_have_an_established_namespace(self):
        self.assertEqual(self.keys('fn pane() { t("caption"); t("UpperLabel"); t("under_score"); }'),
                         {"caption", "UpperLabel", "under_score"})

    def test_comments_test_items_bytes_and_dynamic_templates_are_not_copy_keys(self):
        source = '''
        // t("settings-comment")
        /* t("settings-block-comment") */
        #[cfg(test)] mod tests { fn fixture() { t("settings-test-copy"); } }
        #[test] fn test_only() { t("settings-test-function"); }
        fn pane() {
            t(r#"settings-raw-label"#);
            bytes(b"settings-bytes");
            ffi(c"settings-c-string");
            t(&format!("{key}-mode"));
        }
        '''
        self.assertEqual(self.keys(source), {"settings-raw-label"})

    @staticmethod
    def crate(root, name, locales, source):
        crate = root / name
        (crate / "src").mkdir(parents=True)
        (crate / "locales").mkdir()
        for locale, messages in locales.items():
            (crate / f"locales/{locale}.ftl").write_text(messages)
        (crate / "src/view.rs").write_text(source)

    def test_an_absent_used_key_fails_in_every_shipped_locale(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.crate(root, "crates/sdk/ui-core", {"en-US": "ok = OK\n"}, "")
            present = "settings-present = Present\n"
            self.crate(root, "apps/settings", {"en-US": present, "de-DE": present},
                       'fn pane() { t("settings-absent"); }\n')
            self.crate(root, "apps/slides", {"en-US": "slides-tip = Tip\n"},
                       'fn tip() { tr("slides-tip"); tr("slides-tip-overview"); }\n')
            self.assertEqual(copy.missing_copy(root), [
                "apps/settings/src/view.rs:1: de-DE lacks settings-absent",
                "apps/settings/src/view.rs:1: en-US lacks settings-absent",
                "apps/slides/src/view.rs:1: en-US lacks slides-tip-overview",
            ])

    def test_toolkit_messages_resolve_in_every_crate(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.crate(root, "crates/sdk/ui-core", {"en-US": "history-title = History\n"}, "")
            self.crate(root, "apps/notes", {"en-US": "notes-x = X\n", "de": "notes-x = X\n"},
                       'fn title() { t("history-title"); }\n')
            self.assertEqual(copy.missing_copy(root), [])

    def test_compared_states_inside_a_resolver_are_not_keys(self):
        source = '''
        fn row() {
            t(if id == "nextcloud" { "net-nextcloud-add" } else { "net-other-add" });
            t(if "stale" != state { "net-fresh" } else { "net-stale" });
        }
        '''
        self.assertEqual(self.keys(source),
                         {"net-nextcloud-add", "net-other-add", "net-fresh", "net-stale"})

    def test_settings_resolves_the_shared_provider_catalogue(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.crate(root, "crates/sdk/ui-core", {"en-US": "ok = OK\n"}, "")
            shared = "agent-cancel = Cancel\n"
            self.crate(root, "crates/sdk/provider-settings-ui",
                       {"en-US": shared, "de-DE": shared}, "")
            own = "settings-x = X\n"
            self.crate(root, "apps/settings-core", {"en-US": own, "de-DE": own},
                       'fn pane() { t("agent-cancel"); t("agent-absent"); }\n')
            self.assertEqual(copy.missing_copy(root), [
                "apps/settings-core/src/view.rs:1: de-DE lacks agent-absent",
                "apps/settings-core/src/view.rs:1: en-US lacks agent-absent",
            ])

    def test_settings_panes_resolve_the_core_catalogue(self):
        # The panes, in the app and in settings-rare, have no catalogue of
        # their own: their keys are settings-core's.
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.crate(root, "crates/sdk/ui-core", {"en-US": "ok = OK\n"}, "")
            own = "settings-x = X\n"
            self.crate(root, "apps/settings-core", {"en-US": own}, "")
            rare = root / "apps/settings-rare/src"
            rare.mkdir(parents=True)
            (rare / "pane.rs").write_text('fn pane() { t("settings-x"); t("settings-gone"); }\n')
            self.assertEqual(copy.missing_copy(root), [
                "apps/settings-rare/src/pane.rs:1: en-US lacks settings-gone",
            ])

    def test_every_actual_source_key_is_in_its_catalogues(self):
        self.assertEqual(copy.missing_copy(copy.ROOT), [])


if __name__ == "__main__":
    unittest.main()
