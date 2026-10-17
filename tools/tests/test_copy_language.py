import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "check_copy_language", TOOLS / "policy/check_copy_language.py")
copy = importlib.util.module_from_spec(spec)
sys.modules["check_copy_language"] = copy
spec.loader.exec_module(copy)

GLOSSARY = '''
[[term]]
reason = "One secret, one name."
en = { use = "Password", avoid = ["Passcode"] }
de = { use = "Passwort", avoid = ["Kenncode"] }

[[term]]
reason = "The power panel."
en = { use = "Power", avoid = ["Session"], keys = "^power-" }
'''


def rules(locale, text, glossary=GLOSSARY):
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "glossary.toml"
        path.write_text(glossary)
        terms = copy.load_glossary(path)
    return [
        (message.key, rule)
        for message in copy.messages(text)
        for rule, _ in copy.violations(locale, message, terms)
    ]


class CopyLanguageTests(unittest.TestCase):
    def test_each_rule_catches_its_class(self):
        text = "\n".join([
            "a = Your files appear here",
            "b = This window belongs to an app the compositor doesn't host",
            "c = The workspace document did not accept the change",
            "d = Online · identity-certified",
            "e = Action job { $job } was admitted",
            "f = surface { $surface }",
            "g = AGENT CONTROLS · { $name }",
            "h = Direct when reachable · no vendor cloud · trusted only",
            "i = Streaming with { $peer } · select to stop",
            "window-transfer-confirm = Continue",
            "j = Enter your passcode.",
            "power-menu-title = Session",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [
            ("a", "narration"),
            ("b", "implementation"),
            ("c", "implementation"),
            ("d", "implementation"),
            ("e", "implementation"),
            ("e", "raw-id"),
            ("f", "implementation"),
            ("f", "raw-id"),
            ("g", "all-caps"),
            ("h", "chain"),
            ("i", "instruction"),
            ("window-transfer-confirm", "vague-button"),
            ("j", "glossary"),
            ("power-menu-title", "glossary"),
        ])

    def test_plain_copy_passes(self):
        text = "\n".join([
            "a = Couldn't move the tab. Try again.",
            "b = Online · 80%",
            "c = Wi-Fi, VPN and USB over HDMI",
            "d = Tap to click",
            "e = Click a window to focus it",
            "f = Hardware H.264/HEVC is ready",
            "g = Set HOME or XDG_STATE_HOME",
            "h = { $count ->",
            "        [one] { $count } speaker · playing",
            "       *[other] { $count } speakers · playing",
            "    }",
            "search-kw-power = session, compositor, AGENT",
            "window-transfer-confirm = Move",
            "terminal-session = Session ended",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [])

    def test_german_rules(self):
        text = "\n".join([
            "a = AGENT STEUERT · { $name }",
            "b = Zum Beenden auswählen, um fortzufahren",
            "c = Ein zertifiziertes Gerät",
            "d = Gib deinen Kenncode ein",
            "x-confirm = Weiter",
            "e = Als Agent zugelassen",
        ]) + "\n"
        self.assertEqual(rules("de-DE", text), [
            ("a", "all-caps"),
            ("b", "instruction"),
            ("c", "implementation"),
            ("d", "glossary"),
            ("x-confirm", "vague-button"),
        ])

    def test_attributes_and_terms_are_checked(self):
        text = "a = Fine\n    .title = DESKTOP ESSENTIALS\n-brand = Tilo will keep it\n"
        self.assertEqual(rules("en-US", text), [("a.title", "all-caps"), ("-brand", "narration")])

    def test_baseline_only_shrinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = Saved\n")
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            shard = baseline / "apps-demo.txt"

            self.assertEqual(len(copy.check(root, glossary, baseline)), 1)

            shard.write_text("# old copy\nen-US old all-caps\n")
            self.assertEqual(copy.check(root, glossary, baseline), [])

            (locales / "en-US.ftl").write_text("old = Where you left off\nfine = Saved\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer applies", errors[0])

            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = SAVED NOW\n")
            shard.write_text("en-US old all-caps\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("fine", errors[0])

    def test_glossary_terms_need_a_reason(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "glossary.toml"
            path.write_text('[[term]]\nen = { use = "A", avoid = ["B"] }\n')
            with self.assertRaises(ValueError):
                copy.load_glossary(path)

    def test_the_tree_passes(self):
        self.assertEqual(copy.check(copy.ROOT, copy.GLOSSARY, copy.BASELINE), [])


if __name__ == "__main__":
    unittest.main()
