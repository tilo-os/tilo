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
            let mut delete = ActionSpec::operation("remove", "Remove")
                .of_kind(ActionKind::Undoable);
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
                kind: ActionKind::Undoable,
                confirmation: ConfirmationPolicy::Phrase { phrase: name.into() },
                ..ActionContract::default()
            }
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertTrue(any("Undo takes back" in error for error in errors), errors)

    def test_sees_an_undoable_kind_set_through_the_builder(self):
        source = """
        fn note() -> ActionSpec {
            let mut spec = ActionSpec::operation("answer", "Answer");
            spec.kind = ActionKind::Undoable;
            spec.contract.confirmation = ConfirmationPolicy::Phrase {
                phrase: spec.label.clone(),
            };
            spec
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertTrue(any("Undo takes back" in error for error in errors), errors)

    def test_ignores_an_undoable_binding_later_named_outside(self):
        source = """
        fn mail() -> ActionSpec {
            let mut spec = ActionSpec::operation("send", "Send");
            spec.kind = ActionKind::Undoable;
            spec.kind = ActionKind::Outside;
            spec.contract.confirmation = ConfirmationPolicy::Phrase {
                phrase: spec.label.clone(),
            };
            spec
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertEqual(errors, [])

    def test_allows_a_phrase_on_an_action_that_reaches_outside(self):
        source = """
        fn mail() -> ActionSpec {
            let mut spec = ActionSpec::operation("send", "Send")
                .of_kind(ActionKind::Outside);
            spec.contract.confirmation = ConfirmationPolicy::Phrase {
                phrase: spec.label.clone(),
            };
            spec
        }
        """
        errors = check.check_source("apps/x/src/actions.rs", source)
        self.assertEqual(errors, [])

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

    def test_allows_a_deletion_that_asks_for_the_name_of_what_it_erases(self):
        """An action Undo cannot take back says what it destroys, and the
        person types that: a drive, a person, a repository."""
        source = """
        fn erase(device: &str) -> ActionSpec {
            let mut spec = ActionSpec::operation("erase", "Erase drive")
                .destructive()
                .with_contract(ActionContract {
                    tags: vec![tilo_actions::IRREVERSIBLE_TAG.to_owned()],
                    confirmation: ConfirmationPolicy::Phrase { phrase: device.to_owned() },
                    ..ActionContract::default()
                });
            spec
        }
        """
        self.assertEqual(check.check_source("apps/x/src/actions.rs", source), [])

    def test_tests_may_name_the_old_word(self):
        source = """
        #[cfg(test)]
        mod tests {
            #[test]
            fn old() { host.value("CONFIRM"); }
        }
        """
        self.assertEqual(check.check_source("crates/x/src/lib.rs", source), [])

    # Undo is a system service: a deletion it takes back runs at once, so a
    # destructive action that is not undoable says why in a tag or in the
    # ledger, where a reviewer reads the reason beside it.
    LEDGER = [
        {
            "action": "erase",
            "file": "apps/disks/src/resource.rs",
            "reason": (
                "The drive's contents belong to whoever used it before, and no "
                "document holds them for Undo to bring back."
            ),
        }
    ]

    def test_refuses_a_deletion_no_undo_takes_back(self):
        source = """
        fn erase(device: &str) -> ActionSpec {
            let mut spec = ActionSpec::operation("erase", "Erase drive", OperationKind::Effect, OP, &"erase")
                .expect("the erase serializes");
            spec.contract.kind = ActionKind::Outside;
            spec = spec.destructive();
            spec
        }
        """
        errors = check.check_source("apps/disks/src/resource.rs", source)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`erase`, a deletion", errors[0])
        self.assertIn("undoable-deletions.toml", errors[0])

    def test_a_deletion_in_a_list_of_actions_reads_its_own_element(self):
        source = """
        pub(crate) const ALL: &'static [Action] = &[
            chrome("tab.new", "label", "subtitle", "keywords", |_, _| Ok(())),
            Action {
                id: "app.uninstall",
                label: "search-action-app-uninstall",
                ..Action::NEW
            }
            .destructive(),
            chrome("tab.close", "label", "subtitle", "keywords", |_, _| Ok(())),
        ];
        """
        errors = check.check_source("crates/session/shell/src/shell/actions/all.rs", source)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`app.uninstall`, a deletion", errors[0])

    def test_an_action_id_named_by_a_constant_reads_the_constant(self):
        # An `Action` names its id with the constant the module that owns the
        # action declares, which is not the file that lists it.
        self.assertEqual(check.string_constant("APP_UNINSTALL_ID"), "app.uninstall")
        source = """
        pub(crate) const ALL: &'static [Action] = &[
            Action {
                id: APP_UNINSTALL_ID,
                ..Action::NEW
            }
            .destructive(),
        ];
        """
        errors = check.check_source("crates/session/shell/src/shell/actions/all.rs", source)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`app.uninstall`, a deletion", errors[0])

    def test_allows_a_deletion_marked_irreversible(self):
        source = """
        fn forget() -> ActionSpec {
            ActionSpec::operation("forget", "Forget account", OperationKind::Effect, OP, &"forget")
                .unwrap()
                .with_contract(ActionContract {
                    tags: vec![tilo_actions::IRREVERSIBLE_TAG.to_owned()],
                    ..ActionContract::default()
                })
                .destructive()
        }
        """
        self.assertEqual(check.check_source("apps/accounts/src/resource.rs", source), [])

    def test_allows_a_deletion_the_ledger_names_at_its_site(self):
        source = """
        fn erase(device: &str) -> ActionSpec {
            ActionSpec::operation("erase", "Erase drive", OperationKind::Effect, OP, &"erase")
                .unwrap()
                .destructive()
        }
        """
        self.assertEqual(
            check.check_source("apps/disks/src/resource.rs", source, self.LEDGER), []
        )

    def test_an_entry_covers_one_site(self):
        source = """
        fn erase(device: &str) -> ActionSpec {
            ActionSpec::operation("erase", "Erase drive", OperationKind::Effect, OP, &"erase")
                .unwrap()
                .destructive()
        }
        """
        errors = check.check_source("apps/other/src/resource.rs", source, self.LEDGER)
        self.assertEqual(len(errors), 1, errors)

    def test_sees_a_deletion_that_closes_a_declaration_builder(self):
        source = """
        fn declaration(&self) -> Option<ActionDecl<Calendar>> {
            let declaration = Calendar::INVITE
                .bind(params.event, crate::model::key(self.event))
                .declaration(Calendar::INVITE.id, t("calendar-contact-add"))?;
            Some(declaration.describe(self.label()).destructive())
        }
        """
        errors = check.check_source("apps/calendar/src/app/contact_attendees.rs", source)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("`invite`, a deletion", errors[0])

    def test_an_action_constant_names_the_deletion(self):
        source = """
        fn row() -> ActionSpec {
            let mut spec = Workspace::DELETE_OFFLINE_AREA.spec();
            spec.kind = ActionKind::Outside;
            spec.destructive()
        }
        """
        entries = [{"action": "delete-offline-area", "file": "apps/maps/src/resource.rs", "reason": "x" * 60}]
        self.assertEqual(
            check.check_source("apps/maps/src/resource.rs", source, entries), []
        )

    def test_ignores_the_word_on_something_that_is_not_a_deletion(self):
        source = """
        fn icon(kind: ChangeKind, action: &ActionSpec) -> Icon {
            let changing = kind.destructive();
            let closing = ToolbarAction::new("call-leave", "phone-off", t("chat-call-leave-action"))
                .destructive();
            let asks = action.is_destructive();
            menu_entry(changing, closing, asks)
        }
        """
        self.assertEqual(check.check_source("apps/chat/src/window_bar.rs", source), [])

    def test_a_ledger_file_covers_the_files_its_pattern_holds(self):
        source = """
        fn erase(device: &str) -> ActionSpec {
            ActionSpec::operation("erase", "Erase drive", OperationKind::Effect, OP, &"erase")
                .unwrap()
                .destructive()
        }
        """
        entries = [dict(self.LEDGER[0], file="apps/disks/src/*.rs")]
        self.assertEqual(
            check.check_source("apps/disks/src/resource.rs", source, entries), []
        )
        errors = check.check_source("apps/other/src/resource.rs", source, entries)
        self.assertEqual(len(errors), 1, errors)
        errors = check.check_source("crates/x/src/resource.rs", source, entries)
        self.assertEqual(len(errors), 1, errors)

    def test_is_settled_by_a_builder_that_settles_its_confirmation(self):
        # A builder generic over the declaration publishes every verb with the
        # undo its declaration declares, so the tag settles it again.
        source = """
        fn finish(mut spec: ActionSpec, decl: &Declaration) -> ActionSpec {
            for tag in &decl.tags {
                spec.contract.tags.push(tag.clone());
            }
            if decl.destructive {
                spec = spec.destructive().with_tagged_undo();
            }
            spec
        }
        """
        self.assertEqual(check.check_source("crates/x/src/items.rs", source), [])

    def test_is_settled_by_a_builder_that_tags_the_spec_it_returns(self):
        # A builder puts the declaration's tags on the spec before handing it
        # back, and `destructive` settles from what the spec carries, so the
        # undo the declaration declares decides. A deletion outside such a
        # builder reads no tags, and is refused.
        tagged = """
        fn finish(mut spec: ActionSpec, decl: &Declaration) -> ActionSpec {
            for tag in &decl.tags {
                spec.contract.tags.push(tag.clone());
            }
            if decl.destructive {
                spec = spec.destructive();
            }
            spec
        }
        """
        self.assertEqual(check.check_source("crates/x/src/items.rs", tagged), [])
        untagged = """
        fn finish(mut spec: ActionSpec) -> ActionSpec {
            spec = spec.destructive();
            spec
        }

        fn other(spec: &mut ActionSpec) {
            spec.contract.tags.push("undo:compensated".into());
        }
        """
        errors = check.check_source("crates/x/src/items.rs", untagged)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("a deletion the code does not name", errors[0])

    def test_refuses_a_ledger_entry_without_a_reason(self):
        errors = []
        kept = check.ledger_entries(
            [{"action": "erase", "file": "apps/x/src/resource.rs"}], errors
        )
        self.assertEqual(kept, [])
        self.assertTrue(any("gives no reason" in error for error in errors), errors)

    def test_refuses_a_ledger_entry_that_pins_nothing(self):
        errors = []
        kept = check.ledger_entries(
            [
                {
                    "action": "erase",
                    "reason": "the drive holds data no document keeps for Undo",
                }
            ],
            errors,
        )
        self.assertEqual(kept, [])
        self.assertTrue(any("pins the site" in error for error in errors), errors)

    def test_refuses_a_ledger_entry_nothing_builds(self):
        errors = check.unbuilt(
            [dict(self.LEDGER[0], action="forget")],
            [("apps/disks/src/resource.rs", "erase", set())],
        )
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("no deletion named `forget`", errors[0])


if __name__ == "__main__":
    unittest.main()
