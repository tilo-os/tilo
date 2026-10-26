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


if __name__ == "__main__":
    unittest.main()
