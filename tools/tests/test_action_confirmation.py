import importlib.util
import sys
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS / "policy"))
spec = importlib.util.spec_from_file_location(
    "check_action_confirmation", TOOLS / "policy/check_action_confirmation.py"
)
check = importlib.util.module_from_spec(spec)
sys.modules["check_action_confirmation"] = check
spec.loader.exec_module(check)


class ActionConfirmationTests(unittest.TestCase):
    # A typed confirmation is for irreversible, high-risk actions, and its
    # word names what is lost; the policy refuses one on an undoable action.
    def test_refuses_a_typed_phrase_on_an_undoable_action(self):
        source = """
        fn remove(app: &App) -> ActionSpec {
            let mut delete = ActionSpec::operation("remove", "Remove").reversible();
            delete.contract.confirmation = ConfirmationPolicy::Phrase {
                phrase: app.name.clone(),
            };
            delete
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("Undo takes back", errors[0])

    def test_refuses_a_phrase_in_an_undoable_contract_literal(self):
        source = """
        fn contract() -> ActionContract {
            ActionContract {
                reversible: true,
                confirmation: ConfirmationPolicy::Phrase { phrase: name.into() },
                ..ActionContract::default()
            }
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertTrue(any("Undo takes back" in error for error in errors), errors)

    def test_refuses_generic_words_and_any_confirm_literal(self):
        source = """
        fn erase() {
            spec.contract.confirmation = ConfirmationPolicy::Phrase { phrase: "Delete".into() };
            let expected = "CONFIRM";
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertTrue(any('"Delete" says nothing' in error for error in errors), errors)
        self.assertTrue(any('"CONFIRM" is never' in error for error in errors), errors)

    def test_allows_an_irreversible_action_that_asks_for_its_name(self):
        source = """
        fn erase(device: &str) -> ActionSpec {
            let mut spec = ActionSpec::operation("erase", "Erase drive").destructive();
            spec.contract.confirmation = ConfirmationPolicy::Phrase {
                phrase: device.to_owned(),
            };
            spec
        }
        """
        self.assertEqual(check.check_source("apps/disks/src/resource.rs", source), [])

    def test_tests_may_name_the_old_word(self):
        source = """
        #[cfg(test)]
        mod tests {
            #[test]
            fn old() { host.value("CONFIRM"); }
        }
        """
        self.assertEqual(check.check_source("crates/x/src/lib.rs", source), [])



# Every destructive action of the apps and SDK that asked before Undo took
# over: each asks now (`irreversible`) or Undo takes it back in full (listed,
# with why, in tools/policy/undoable-deletions.toml).
ASKED_BEFORE_UNDO = [
    "apps/api-tester/src/resource.rs::delete_collection",
    "apps/api-tester/src/resource.rs::delete_environment",
    "apps/api-tester/src/resource.rs::delete_request",
    "apps/calculator/src/resource.rs::clear_history",
    "apps/calculator/src/resource.rs::delete_function",
    "apps/calculator/src/resource.rs::delete_history_entry",
    "apps/calculator/src/resource.rs::delete_surface",
    "apps/calendar/src/resource.rs::delete_project",
    "apps/chat/src/messages.rs::delete_message",
    "apps/chat/src/messages.rs::remove_message",
    "apps/chat/src/record.rs::delete_category",
    "apps/chat/src/record.rs::delete_channel",
    "apps/code-editor/src/project.rs::delete",
    "apps/contacts/src/resource.rs::delete_contact",
    "apps/contacts/src/resource.rs::merge_contacts",
    "apps/db-client/src/resource.rs::remove_connection",
    "apps/db-client/src/resource.rs::remove_query",
    "apps/ebook/src/resource.rs::delete_scheme",
    "apps/ebook/src/resource.rs::remove_font",
    "apps/ebook/src/resource.rs::remove_from_library",
    "apps/feeds/src/resource.rs::delete_folder",
    "apps/feeds/src/resource.rs::unsubscribe",
    "apps/firewall/src/resource.rs::enter_panic_mode",
    "apps/firewall/src/resource.rs::stop",
    "apps/hex-editor/src/resource.rs::delete_bytes",
    "apps/mail/src/resource.rs::discard_draft",
    "apps/mail/src/resource.rs::purge",
    "apps/mail/src/resource.rs::send_message",
    "apps/music/src/resource.rs::delete_playlist",
    "apps/notes/src/notebook.rs::delete",
    "apps/notes/src/notebook.rs::delete_tag",
    "apps/photos/src/resource.rs::remove_album",
    "apps/photos/src/resource.rs::remove_person",
    "apps/photos/src/resource.rs::remove_photo",
    "apps/remote/src/resource.rs::delete_connection",
    "apps/sheets/src/resource.rs::delete_chart",
    "apps/sheets/src/resource.rs::delete_columns",
    "apps/sheets/src/resource.rs::delete_comment",
    "apps/sheets/src/resource.rs::delete_rows",
    "apps/sheets/src/resource.rs::delete_sheet",
    "apps/slides/src/deck.rs::delete_element",
    "apps/slides/src/deck.rs::delete_slide",
    "apps/snippets/src/resource.rs::delete",
    "apps/speed-test/src/resource.rs::clear",
    "apps/tasks/src/model/record.rs::delete",
    "apps/themes/src/studio.rs::delete",
    "apps/totp/src/resource.rs::delete",
    "apps/typst-editor/src/resource.rs::close_file",
    "apps/video-editor/src/resource.rs::clear_effects",
    "apps/video-editor/src/resource.rs::close_gap",
    "apps/video-editor/src/resource.rs::delete_marker",
    "apps/video-editor/src/resource.rs::delete_track",
    "apps/video-editor/src/resource.rs::remove_clip",
    "apps/video-editor/src/resource.rs::remove_clip_closing",
    "apps/video-editor/src/resource.rs::remove_effect",
    "apps/video-editor/src/resource.rs::remove_media",
    "apps/video-player/src/shared.rs::delete_annotation",
    "apps/video-player/src/shared.rs::delete_playlist",
    "apps/weather/src/resource.rs::remove",
    "apps/word-processor/src/model.rs::accept_all_changes",
    "apps/word-processor/src/model.rs::delete_column",
    "apps/word-processor/src/model.rs::delete_image",
    "apps/word-processor/src/model.rs::delete_row",
    "apps/word-processor/src/model.rs::delete_table",
    "apps/word-processor/src/model.rs::reject_all_changes",
    "crates/dev/tilo-app-cli/templates/document.rs::clear",
    "crates/platform/tilo-contact-directory/src/lib.rs::confirm_account",
    "crates/platform/tilo-contact-directory/src/lib.rs::confirm_endpoint",
    "crates/platform/tilo-contact-directory/src/lib.rs::forget_endpoint",
    "crates/platform/tilo-contact-directory/src/lib.rs::replace_account",
    "crates/platform/tilo-contact-directory/src/lib.rs::replace_endpoint",
    "crates/runtime/environment/src/lib.rs::remove_tool",
    "crates/runtime/environment/src/lib.rs::revoke_grant",
    "crates/sdk/tilo-app/examples/resource-journey.rs::remove",
    "examples/apps/counter/src/lib.rs::reset",
    "examples/apps/shared-checklist/src/lib.rs::clear_done",
]

# What Undo cannot take back, so it asks first.
ASKS_NOW = {
    "apps/firewall/src/resource.rs::stop",
    "apps/firewall/src/resource.rs::enter_panic_mode",
    "apps/totp/src/resource.rs::delete",
    "apps/mail/src/resource.rs::purge",
    "crates/platform/tilo-contact-directory/src/lib.rs::confirm_endpoint",
    "crates/platform/tilo-contact-directory/src/lib.rs::confirm_account",
    "crates/platform/tilo-contact-directory/src/lib.rs::replace_endpoint",
    "crates/platform/tilo-contact-directory/src/lib.rs::replace_account",
    "apps/db-client/src/resource.rs::remove_connection",
    "apps/ebook/src/resource.rs::remove_from_library",
    "apps/remote/src/resource.rs::delete_connection",
}


class UndoableDeletionTests(unittest.TestCase):
    def test_an_unlisted_destructive_action_with_undo_is_refused(self):
        source = """
        impl Inbox {
            /// Empty the outbox.
            #[action(destructive)]
            pub fn flush(&mut self) {}
        }
        """
        found = dict(check.undoable_deletions("apps/x/src/resource.rs", source))
        self.assertEqual(list(found), ["apps/x/src/resource.rs::flush"])
        errors = check.check_ledger(found, {})
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("irreversible", errors[0])
        self.assertEqual(
            check.check_ledger(found, {"apps/x/src/resource.rs::flush": "Record only."}), []
        )

    def test_an_irreversible_action_asks_and_needs_no_entry(self):
        source = """
        #[action(destructive, irreversible)]
        pub fn purge(&mut self) {}
        #[effect(row, destructive)]
        EmptyTrash,
        """
        self.assertEqual(check.undoable_deletions("apps/x/src/resource.rs", source), [])

    def test_a_stale_entry_is_refused(self):
        errors = check.check_ledger({}, {"apps/x/src/resource.rs::gone": "Record only."})
        self.assertEqual(len(errors), 1, errors)

    def test_every_action_that_asked_before_asks_now_or_undo_takes_it_back(self):
        root = check.ROOT
        ledger = check.tomllib.loads(check.LEDGER.read_text(encoding="utf-8"))["undoable"]
        for key in ASKED_BEFORE_UNDO:
            path, name = key.split("::")
            source = (root / path).read_text(encoding="utf-8")
            found = dict(check.undoable_deletions(path, source))
            if key in ASKS_NOW:
                self.assertNotIn(key, found, f"{key} reaches outside the record: it asks")
                self.assertRegex(
                    source,
                    rf"#\[action\([^\]]*irreversible[^\]]*\)\]\s*(?:///[^\n]*\n\s*|//[^\n]*\n\s*)*pub fn {name}\b",
                    f"{key} is marked irreversible",
                )
            else:
                self.assertIn(key, found, f"{key} still runs at once with Undo")
                self.assertTrue(ledger.get(key, "").strip(), f"{key} says why Undo takes it back")
        # A private contact added to a shared event is seen by everyone it is
        # shared with: Undo cannot take that back, so it asks.
        attendees = (root / "apps/calendar/src/app/contact_attendees.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn("IRREVERSIBLE_TAG", attendees)

if __name__ == "__main__":
    unittest.main()
