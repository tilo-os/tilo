import importlib.util
import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path


TOOLS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(TOOLS / "policy"))
spec = importlib.util.spec_from_file_location(
    "check_catalogue_coverage", TOOLS / "policy/check_catalogue_coverage.py")
coverage = importlib.util.module_from_spec(spec)
sys.modules["check_catalogue_coverage"] = coverage
spec.loader.exec_module(coverage)

SOURCE = """a = Title
    .doc = What it is.

b = Settings
"""


def tree(files: dict[str, str]) -> tempfile.TemporaryDirectory:
    directory = tempfile.TemporaryDirectory()
    for name, text in files.items():
        path = Path(directory.name) / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    return directory


def missing(files: dict[str, str]) -> list[tuple[str, str, str]]:
    with tree(files) as root:
        return [(unit, locale, key)
                for unit, locale, _, key, _ in coverage.untranslated(Path(root))]


def run(files: dict[str, str], *arguments: str) -> tuple[int, str, str]:
    with tree(files) as root:
        original = coverage.ROOT
        coverage.ROOT = Path(root)
        out, err = io.StringIO(), io.StringIO()
        try:
            with redirect_stdout(out), redirect_stderr(err):
                code = coverage.main(["check", *arguments])
        finally:
            coverage.ROOT = original
        return code, out.getvalue(), err.getvalue()


class CatalogueCoverageTests(unittest.TestCase):
    def test_a_key_only_the_source_has_is_reported(self):
        found = missing({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n    .doc = Was es ist.\n",
        })
        self.assertEqual(found, [("app", "de-DE", "b")])

    def test_an_attribute_is_a_key_of_its_own(self):
        found = missing({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n\nb = Einstellungen\n",
        })
        self.assertEqual(found, [("app", "de-DE", "a.doc")])

    def test_a_locale_may_add_its_own_words(self):
        self.assertEqual(missing({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n    .doc = Was es ist.\n\nb = Einstellungen\n"
                                    "\nc = Nur auf Deutsch\n",
        }), [])

    def test_a_complete_translation_passes(self):
        self.assertEqual(missing({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n    .doc = Was es ist.\n\nb = Einstellungen\n",
        }), [])

    def test_the_listing_names_each_untranslated_key_and_passes(self):
        code, out, _ = run({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n    .doc = Was es ist.\n",
        }, "--list")
        self.assertEqual(code, 0)
        self.assertEqual(out, "app de-DE b\n")

    def test_a_partly_translated_catalogue_fails_and_names_the_key(self):
        code, out, err = run({
            "app/locales/en-US.ftl": SOURCE,
            "app/locales/de-DE.ftl": "a = Titel\n    .doc = Was es ist.\n",
        })
        self.assertEqual(code, 1)
        self.assertEqual(out, "app/locales/en-US.ftl:4: b has no de-DE text\n")
        self.assertIn("1 keys are untranslated", err)


if __name__ == "__main__":
    unittest.main()
