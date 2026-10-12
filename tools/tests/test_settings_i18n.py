import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
for name in ("check_app_portability", "check_settings_i18n"):
    spec = importlib.util.spec_from_file_location(name, TOOLS / f"policy/{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
copy = sys.modules["check_settings_i18n"]


class SettingsCopyTests(unittest.TestCase):
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

    def test_an_absent_used_key_fails_even_when_existing_messages_are_valid(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            settings = root / "apps/settings"
            (settings / "src").mkdir(parents=True)
            (settings / "locales").mkdir()
            for locale in ("en-US", "de-DE"):
                (settings / f"locales/{locale}.ftl").write_text("settings-present = Present\n")
            (settings / "src/pane.rs").write_text('fn pane() { t("settings-absent"); }\n')
            self.assertEqual(copy.missing_copy(root), [
                "apps/settings/src/pane.rs:1: en-US lacks settings-absent",
                "apps/settings/src/pane.rs:1: de-DE lacks settings-absent",
            ])

    def test_every_actual_settings_source_key_has_both_catalogue_entries(self):
        self.assertEqual(copy.missing_copy(copy.ROOT), [])


if __name__ == "__main__":
    unittest.main()
