#!/usr/bin/env python3
"""Validate the package structure of a bounded DOCX export, not its rendering.

Used by the opt-in Rust twin contract; requires only Python's standard library.
Run with --self-test for credential-free checks of the validator itself.
"""
import io
import sys
import unittest
import zipfile
from xml.etree import ElementTree as ET

LIMIT = 10 * 1024 * 1024
CONTENT_TYPES = "http://schemas.openxmlformats.org/package/2006/content-types"
RELATIONSHIPS = "http://schemas.openxmlformats.org/package/2006/relationships"
OFFICE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument"
WORD = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
DOCX = "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"


def validate(data):
    if len(data) > LIMIT:
        raise ValueError("DOCX export exceeds the 10 MiB contract limit")
    if not zipfile.is_zipfile(io.BytesIO(data)):
        raise ValueError("DOCX export is not a ZIP Office Open XML package (stand-in bytes)")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        if sum(entry.file_size for entry in entries) > LIMIT:
            raise ValueError("DOCX expanded package exceeds the contract limit")
        if len({entry.filename for entry in entries}) != len(entries):
            raise ValueError("DOCX package contains duplicate entries")
        if archive.testzip() is not None:
            raise ValueError("DOCX ZIP integrity check failed")
        try:
            types = ET.fromstring(archive.read("[Content_Types].xml"))
            relationships = ET.fromstring(archive.read("_rels/.rels"))
            document = ET.fromstring(archive.read("word/document.xml"))
        except (KeyError, ET.ParseError) as error:
            raise ValueError("DOCX is missing required package parts or valid XML") from error
        if types.tag != f"{{{CONTENT_TYPES}}}Types" or not any(
            entry.get("PartName") == "/word/document.xml"
            and entry.get("ContentType") == DOCX
            for entry in types.findall(f"{{{CONTENT_TYPES}}}Override")
        ):
            raise ValueError("DOCX lacks the Word document content-type declaration")
        if relationships.tag != f"{{{RELATIONSHIPS}}}Relationships" or not any(
            entry.get("Type") == OFFICE
            and entry.get("Target") in ("word/document.xml", "/word/document.xml")
            and entry.get("TargetMode", "Internal") == "Internal"
            for entry in relationships.findall(f"{{{RELATIONSHIPS}}}Relationship")
        ):
            raise ValueError("DOCX lacks the internal Office document relationship")
        if document.tag != f"{{{WORD}}}document" or document.find(f"{{{WORD}}}body") is None:
            raise ValueError("DOCX lacks a WordprocessingML document body")


class ValidatorTests(unittest.TestCase):
    def package(self, **overrides):
        parts = {
            "[Content_Types].xml": f'<Types xmlns="{CONTENT_TYPES}"><Override PartName="/word/document.xml" ContentType="{DOCX}"/></Types>',
            "_rels/.rels": f'<Relationships xmlns="{RELATIONSHIPS}"><Relationship Id="r1" Type="{OFFICE}" Target="word/document.xml"/></Relationships>',
            "word/document.xml": f'<w:document xmlns:w="{WORD}"><w:body/></w:document>',
        }
        parts.update(overrides)
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
            for name, data in parts.items():
                archive.writestr(name, data)
        return buffer.getvalue()

    def test_accepts_structural_word_package(self):
        validate(self.package())

    def test_rejects_stand_ins_and_zip_signature_only(self):
        for data in (b'{"name":"Notes"}', b"Notes exported as DOCX", b"PK\x03\x04not-a-zip"):
            with self.subTest(data=data), self.assertRaisesRegex(ValueError, "not a ZIP"):
                validate(data)

    def test_rejects_zip_without_office_parts(self):
        buffer = io.BytesIO()
        with zipfile.ZipFile(buffer, "w") as archive:
            archive.writestr("placeholder.txt", "Notes")
        with self.assertRaisesRegex(ValueError, "required package parts"):
            validate(buffer.getvalue())

    def test_rejects_invalid_office_xml(self):
        for name, data in (
            ("word/document.xml", "not XML"),
            ("word/document.xml", "<document><body/></document>"),
            ("[Content_Types].xml", f'<Types xmlns="{CONTENT_TYPES}"/>'),
            ("_rels/.rels", f'<Relationships xmlns="{RELATIONSHIPS}"/>'),
        ):
            with self.subTest(name=name), self.assertRaises(ValueError):
                validate(self.package(**{name: data}))


if __name__ == "__main__":
    if sys.argv[1:] == ["--self-test"]:
        unittest.main(argv=[sys.argv[0]])
    else:
        try:
            validate(sys.stdin.buffer.read(LIMIT + 1))
        except (ValueError, zipfile.BadZipFile, RuntimeError) as error:
            sys.exit(f"native Office export contract failed: {error}")
