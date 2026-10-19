"""The language packs hold every macro catalogue's translations, by locale and crate."""

from __future__ import annotations

import importlib.util
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("language_packs", ROOT / "tools/apps/language_packs.py")
assert SPEC and SPEC.loader
packs = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(packs)


def crate(root: pathlib.Path, name: str, source: str, locales: dict[str, str]) -> None:
    (root / name / "src").mkdir(parents=True)
    (root / name / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n', encoding="utf-8")
    (root / name / "src/lib.rs").write_text(source, encoding="utf-8")
    (root / name / "locales").mkdir()
    for locale, text in locales.items():
        (root / name / "locales" / f"{locale}.ftl").write_text(text, encoding="utf-8")


class LanguagePackTests(unittest.TestCase):
    def test_translations_of_macro_catalogues_go_to_their_locale_by_crate(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            repo = pathlib.Path(scratch) / "repo"
            crate(repo, "notes", 'ui::embedded_catalog!(pub fn, ["en-US", "de"]);\n',
                  {"en-US": "a = A\n", "de": "a = Ä\n"})
            # A crate that embeds its catalogues itself keeps them.
            crate(repo, "mail", 'static C: EmbeddedCatalog = EmbeddedCatalog::new("en-US", L);\n',
                  {"en-US": "a = A\n", "de-DE": "a = Ä\n"})
            out = pathlib.Path(scratch) / "packs"
            self.assertEqual(packs.build(repo, out), 0)
            self.assertEqual((out / "de" / "notes.ftl").read_text(encoding="utf-8"), "a = Ä\n")
            self.assertFalse((out / "en-US").exists(), "English stays in the packages")
            self.assertFalse((out / "de-DE").exists(), "a self-embedding catalogue is not packed")

    def test_the_repository_packs_settings_german(self) -> None:
        found = {(locale, name) for locale, name, _ in packs.catalogues(ROOT)}
        self.assertIn(("de-DE", "settings"), found)
        self.assertNotIn("en-US", {locale for locale, _ in found})


if __name__ == "__main__":
    unittest.main()
