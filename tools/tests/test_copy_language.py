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
            ("d", "glyph"),
            ("e", "implementation"),
            ("e", "raw-id"),
            ("f", "implementation"),
            ("f", "raw-id"),
            ("g", "all-caps"),
            ("g", "glyph"),
            ("h", "chain"),
            ("h", "glyph"),
            ("i", "glyph"),
            ("i", "instruction"),
            ("window-transfer-confirm", "vague-button"),
            ("j", "glossary"),
            ("power-menu-title", "glossary"),
        ])

    def test_plain_copy_passes(self):
        text = "\n".join([
            "a = Couldn't move the tab. Try again.",
            "b = Online, 80%",
            "c = Wi-Fi, VPN and USB over HDMI",
            "d = Tap to click",
            "e = Click a window to focus it",
            "f = Hardware H.264/HEVC is ready",
            "g = Set HOME or XDG_STATE_HOME",
            "h = { $count ->",
            "        [one] { $count } speaker playing",
            "       *[other] { $count } speakers playing",
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
            ("a", "glyph"),
            ("b", "instruction"),
            ("c", "implementation"),
            ("d", "glossary"),
            ("x-confirm", "vague-button"),
        ])

    def test_separator_glyphs_and_symbol_icons_are_refused(self):
        text = "\n".join([
            "a = { $who } · { $when }",
            "b = Enter to save • Esc to cancel",
            "c = Settings | Network",
            "d = Offline — reconnecting",
            "e = Offline – reconnecting",
            "f = { $done } / { $total }",
            "g = ✓ Connected",
            "h = Next →",
            "i = ★ Favourites",
            "ok-range = Mon–Fri",
            "ok-pair = Read/write",
            "ok-sentence = Your files are safe, and the copy is kept.",
            "ok-dimension = { $w }×{ $h }",
        ]) + "\n"
        self.assertEqual(rules("en-US", text), [
            ("a", "glyph"),
            ("b", "glyph"),
            ("c", "glyph"),
            ("d", "glyph"),
            ("e", "glyph"),
            ("f", "glyph"),
            ("g", "glyph"),
            ("h", "glyph"),
            ("i", "glyph"),
        ])

    def test_attributes_and_terms_are_checked(self):
        text = "a = Fine\n    .title = DESKTOP ESSENTIALS\n-brand = Tilo will keep it\n"
        self.assertEqual(rules("en-US", text), [("a.title", "all-caps"), ("-brand", "narration")])

    def test_baseline_only_shrinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = Synced\n")
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            shard = baseline / "apps-demo.txt"

            self.assertEqual(len(copy.check(root, glossary, baseline)), 1)

            shard.write_text("# old copy\nen-US old all-caps\n")
            self.assertEqual(copy.check(root, glossary, baseline), [])

            (locales / "en-US.ftl").write_text("old = Where you left off\nfine = Synced\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer applies", errors[0])

            (locales / "en-US.ftl").write_text("old = YOU LEFT OFF\nfine = SAVED NOW\n")
            shard.write_text("en-US old all-caps\n")
            errors = copy.check(root, glossary, baseline)
            self.assertEqual(len(errors), 1)
            self.assertIn("fine", errors[0])

    def test_routine_saving_is_caught_and_outcomes_pass(self):
        text = "\n".join([
            "a = Saving…",
            "b = Saved",
            "c = Saved { $file }",
            "d = { $file } saved",
            "e = Draft saved",
            "f = Saving latest changes…",
            "g = Saved — waiting for the agent to apply it",
            "h = All changes saved.",
            "ok-copy = Saved a copy as { $name }",
            "ok-button = Save",
            "ok-failure = Couldn't save. Try again.",
            "ok-place = Saved places",
            "ok-where = Saved as { $name }.",
        ]) + "\n"
        expected = [(key, "save-status") for key in "abcdefgh"]
        expected.insert(6, ("g", "glyph"))
        self.assertEqual(rules("en-US", text), expected)
        german = "\n".join([
            "a = Gespeichert.",
            "b = { $file } gespeichert",
            "c = Wird gespeichert…",
            "d = Änderungen werden gespeichert…",
            "ok-button = Speichern",
        ]) + "\n"
        self.assertEqual(rules("de-DE", german), [(key, "save-status") for key in "abcd"])

    def test_save_exceptions_are_narrow_and_shrink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            locales = root / "apps/demo/locales"
            locales.mkdir(parents=True)
            (locales / "en-US.ftl").write_text("demo-saved = Saved\n")
            source = root / "apps/demo/src"
            source.mkdir(parents=True)
            (source / "lib.rs").write_text(
                'fn label() -> &\'static str { "Saving…" }\n'
                'fn id() -> &\'static str { "saved" }\n'
                '#[cfg(test)]\nmod tests { fn t() { let _ = "Saved"; } }\n')
            glossary = root / "glossary.toml"
            glossary.write_text(GLOSSARY)
            baseline = root / "baseline"
            baseline.mkdir()
            exceptions = root / "exceptions.toml"
            exceptions.write_text("")
            errors = copy.check(root, glossary, baseline, exceptions)
            self.assertEqual(len(errors), 2, errors)
            self.assertIn("demo-saved", errors[0])
            self.assertIn("apps/demo/src/lib.rs:1", errors[1])

            exceptions.write_text(
                '[[allow]]\nunit = "apps-demo"\nkey = "demo-saved"\nreason = "A name."\n'
                '[[allow]]\nfile = "apps/demo/src/lib.rs"\ntext = "Saving…"\nreason = "A name."\n')
            self.assertEqual(copy.check(root, glossary, baseline, exceptions), [])

            (locales / "en-US.ftl").write_text("demo-saved = Saved a copy\n")
            errors = copy.check(root, glossary, baseline, exceptions)
            self.assertEqual(len(errors), 1)
            self.assertIn("no longer matches", errors[0])

            exceptions.write_text('[[allow]]\nunit = "apps-demo"\nkey = "demo-saved"\n')
            with self.assertRaises(ValueError):
                copy.check(root, glossary, baseline, exceptions)

    def test_glossary_terms_need_a_reason(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "glossary.toml"
            path.write_text('[[term]]\nen = { use = "A", avoid = ["B"] }\n')
            with self.assertRaises(ValueError):
                copy.load_glossary(path)

    def test_the_tree_passes(self):
        self.assertEqual(
            copy.check(copy.ROOT, copy.GLOSSARY, copy.BASELINE, copy.SAVE_EXCEPTIONS), [])


if __name__ == "__main__":
    unittest.main()
