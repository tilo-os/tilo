#!/usr/bin/env python3
"""Write the trial's source documents, byte for byte the same on every run.

    python3 generate.py OUT_DIR

The checker imports `build_documents` to know each original's exact bytes, so
nothing here may depend on the clock, the locale or randomness.
"""

from __future__ import annotations

import io
import sys
import zipfile
import zlib
from pathlib import Path
from xml.sax.saxutils import escape

import facts
from pdfmini import TextPage, Widget, wrap, write_document

# ----------------------------------------------------------------------------
# A 5x7 bitmap font, for the identity card scans: an image with no text layer,
# as a scanner makes. Upper case, digits and a little punctuation.
# ----------------------------------------------------------------------------

GLYPHS = {
    "A": "01110 10001 10001 11111 10001 10001 10001",
    "B": "11110 10001 10001 11110 10001 10001 11110",
    "C": "01110 10001 10000 10000 10000 10001 01110",
    "D": "11110 10001 10001 10001 10001 10001 11110",
    "E": "11111 10000 10000 11110 10000 10000 11111",
    "F": "11111 10000 10000 11110 10000 10000 10000",
    "G": "01110 10001 10000 10111 10001 10001 01111",
    "H": "10001 10001 10001 11111 10001 10001 10001",
    "I": "01110 00100 00100 00100 00100 00100 01110",
    "J": "00111 00010 00010 00010 00010 10010 01100",
    "K": "10001 10010 10100 11000 10100 10010 10001",
    "L": "10000 10000 10000 10000 10000 10000 11111",
    "M": "10001 11011 10101 10101 10001 10001 10001",
    "N": "10001 10001 11001 10101 10011 10001 10001",
    "O": "01110 10001 10001 10001 10001 10001 01110",
    "P": "11110 10001 10001 11110 10000 10000 10000",
    "Q": "01110 10001 10001 10001 10101 10010 01101",
    "R": "11110 10001 10001 11110 10100 10010 10001",
    "S": "01111 10000 10000 01110 00001 00001 11110",
    "T": "11111 00100 00100 00100 00100 00100 00100",
    "U": "10001 10001 10001 10001 10001 10001 01110",
    "V": "10001 10001 10001 10001 10001 01010 00100",
    "W": "10001 10001 10001 10101 10101 10101 01010",
    "X": "10001 10001 01010 00100 01010 10001 10001",
    "Y": "10001 10001 10001 01010 00100 00100 00100",
    "Z": "11111 00001 00010 00100 01000 10000 11111",
    "0": "01110 10001 10011 10101 11001 10001 01110",
    "1": "00100 01100 00100 00100 00100 00100 01110",
    "2": "01110 10001 00001 00010 00100 01000 11111",
    "3": "11111 00010 00100 00010 00001 10001 01110",
    "4": "00010 00110 01010 10010 11111 00010 00010",
    "5": "11111 10000 11110 00001 00001 10001 01110",
    "6": "00110 01000 10000 11110 10001 10001 01110",
    "7": "11111 00001 00010 00100 01000 01000 01000",
    "8": "01110 10001 10001 01110 10001 10001 01110",
    "9": "01110 10001 10001 01111 00001 00010 01100",
    "-": "00000 00000 00000 11111 00000 00000 00000",
    "/": "00000 00001 00010 00100 01000 10000 00000",
    ".": "00000 00000 00000 00000 00000 01100 01100",
    ",": "00000 00000 00000 00000 01100 00100 01000",
    ":": "00000 01100 01100 00000 01100 01100 00000",
    " ": "00000 00000 00000 00000 00000 00000 00000",
}


class Canvas:
    def __init__(self, width: int, height: int, colour: tuple[int, int, int]) -> None:
        self.width = width
        self.height = height
        self.pixels = bytearray(bytes(colour) * (width * height))

    def rect(self, x: int, y: int, w: int, h: int, colour: tuple[int, int, int]) -> None:
        row = bytes(colour) * max(0, min(w, self.width - x))
        for yy in range(max(0, y), min(self.height, y + h)):
            start = (yy * self.width + x) * 3
            self.pixels[start:start + len(row)] = row

    def disc(self, cx: int, cy: int, r: int, colour: tuple[int, int, int]) -> None:
        for yy in range(cy - r, cy + r + 1):
            for xx in range(cx - r, cx + r + 1):
                if (xx - cx) ** 2 + (yy - cy) ** 2 <= r * r and 0 <= xx < self.width and 0 <= yy < self.height:
                    i = (yy * self.width + xx) * 3
                    self.pixels[i:i + 3] = bytes(colour)

    def text(self, x: int, y: int, text: str, scale: int, colour: tuple[int, int, int]) -> None:
        for ch in text.upper():
            rows = GLYPHS.get(ch, GLYPHS[" "]).split()
            for ry, bits in enumerate(rows):
                for rx, bit in enumerate(bits):
                    if bit == "1":
                        self.rect(x + rx * scale, y + ry * scale, scale, scale, colour)
            x += 6 * scale

    def grain(self, seed: int) -> None:
        """A scanner's faint, deterministic noise."""
        state = seed
        for i in range(0, len(self.pixels), 3):
            state = (state * 1103515245 + 12345) & 0x7FFFFFFF
            delta = (state >> 16) % 9 - 4
            for c in range(3):
                self.pixels[i + c] = max(0, min(255, self.pixels[i + c] + delta))


def id_card_scan(card: dict, seed: int) -> Canvas:
    ink, muted, paper = (28, 32, 40), (88, 98, 112), (226, 232, 236)
    c = Canvas(640, 404, paper)
    c.rect(0, 0, 640, 58, (44, 78, 112))
    c.text(20, 16, "NORTHVALE IDENTITY CARD", 4, (240, 244, 248))
    c.rect(24, 80, 150, 190, (178, 184, 190))
    c.disc(99, 150, 38, (120, 128, 138))
    c.rect(49, 200, 100, 70, (120, 128, 138))
    rows = [
        ("SURNAME", facts.APPLICANT["last_name"]),
        ("GIVEN NAMES", f"{facts.APPLICANT['first_name']} {facts.APPLICANT['middle_name']}"),
        ("DATE OF BIRTH", facts.APPLICANT["date_of_birth_long"]),
        ("CARD NUMBER", card["number"]),
    ]
    y = 80
    for label, value in rows:
        c.text(196, y, label, 2, muted)
        c.text(196, y + 18, value, 3, ink)
        y += 50
    c.text(24, 290, f"ISSUED {card['issued_long']}", 2, ink)
    c.text(300, 290, f"EXPIRES {card['expires_long']}", 2, ink)
    c.text(24, 318, "ADDRESS", 2, muted)
    c.text(24, 336, card["address"], 2, ink)
    c.text(24, 376, "SPECIMEN - FICTIONAL DOCUMENT FOR TESTING", 2, (150, 60, 60))
    c.grain(seed)
    return c


def id_card_pdf(card: dict, seed: int) -> bytes:
    canvas = id_card_scan(card, seed)
    page = TextPage(width=480, height=303)
    return write_document([page], "Scan", images={0: (canvas.width, canvas.height, bytes(canvas.pixels))})


# ----------------------------------------------------------------------------
# Text documents
# ----------------------------------------------------------------------------

FICTION_NOTE = "Fictional document for testing. Northvale and everyone named here are invented."


def footer(page: TextPage, text: str = FICTION_NOTE) -> None:
    page.rule(60, 60, 475, 0.5, 0.6)
    page.text(60, 46, text, 7.5)


def utility_bill() -> bytes:
    bill = facts.UTILITY_BILL
    p = TextPage()
    p.line(bill["supplier"], 16, bold=True)
    p.line("Account statement", 12)
    p.space(10)
    for line in [
        f"{facts.APPLICANT['first_name']} {facts.APPLICANT['middle_name'][0]}. {facts.APPLICANT['last_name']}",
        facts.CURRENT_ADDRESS["line1"],
        facts.CURRENT_ADDRESS["town"],
        facts.CURRENT_ADDRESS["postcode"],
    ]:
        p.line(line, 11)
    p.space(14)
    for label, value in [
        ("Statement date", bill["date"]),
        ("Account number", bill["account"]),
        ("Billing period", bill["period"]),
        ("Water", "31.62"),
        ("Electricity", "52.55"),
        ("Amount due", bill["amount"]),
        ("Contact email on file", facts.APPLICANT["email"]),
        ("Contact phone on file", facts.APPLICANT["phone"]),
    ]:
        p.text(60, p.y, label, 11, bold=True)
        p.text(250, p.y, value, 11)
        p.space(17)
    p.space(10)
    p.para("The amount due will be collected by direct debit on 20/09/2026. Thank you for being a member "
           "of the cooperative.")
    footer(p)
    return write_document([p], "Account statement")


def payslip(slip: dict) -> bytes:
    return write_document([payslip_page(slip)], f"Payslip {slip['period']}")


def payslip_page(slip: dict) -> TextPage:
    p = TextPage()
    p.line(facts.EMPLOYER["name"], 16, bold=True)
    p.line(facts.EMPLOYER["address"], 10)
    p.space(8)
    p.line("Payslip", 14, bold=True)
    p.line(f"Pay period: {slip['period']}", 12)
    p.space(8)
    for label, value in [
        ("Employee", facts.APPLICANT["full_name"]),
        ("Employee number", facts.EMPLOYER["employee_number"]),
        ("Job title", facts.EMPLOYER["job_title"]),
        ("Employed since", facts.EMPLOYER["start"]),
        ("Paid on", slip["paid"]),
        ("Gross pay", slip["gross"]),
        ("Income tax", slip["tax"]),
        ("Social insurance", slip["insurance"]),
        ("Net pay", slip["net"]),
    ]:
        p.text(60, p.y, label, 11, bold=True)
        p.text(250, p.y, value, 11)
        p.space(17)
    footer(p)
    return p


def employer_letter_pdf(corrected: bool = False) -> bytes:
    p = TextPage()
    paragraphs = facts.letter_paragraphs(corrected)
    p.line(paragraphs[0], 14, bold=True)
    p.line(paragraphs[1], 10)
    p.space(18)
    p.line(paragraphs[2], 11)
    p.space(10)
    p.line(paragraphs[3], 11)
    p.space(6)
    p.line(paragraphs[4], 12, bold=True)
    p.space(4)
    for text in paragraphs[5:]:
        p.para(text)
    footer(p)
    return write_document([p], "Confirmation of employment")


def docx(paragraphs: list[str], bold: set[int]) -> bytes:
    """A minimal WordprocessingML document, deterministic to the byte."""
    body = []
    for index, text in enumerate(paragraphs):
        props = "<w:rPr><w:b/></w:rPr>" if index in bold else ""
        body.append(f'<w:p><w:r>{props}<w:t xml:space="preserve">{escape(text)}</w:t></w:r></w:p>')
    document = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
        '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        f"<w:body>{''.join(body)}<w:sectPr/></w:body></w:document>"
    )
    parts = {
        "[Content_Types].xml": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
            '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
            '<Default Extension="xml" ContentType="application/xml"/>'
            '<Override PartName="/word/document.xml" '
            'ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>'
            "</Types>"
        ),
        "_rels/.rels": (
            '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
            '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
            '<Relationship Id="rId1" '
            'Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" '
            'Target="word/document.xml"/></Relationships>'
        ),
        "word/document.xml": document,
    }
    out = io.BytesIO()
    with zipfile.ZipFile(out, "w") as archive:
        for name, text in parts.items():
            info = zipfile.ZipInfo(name, date_time=(2026, 10, 12, 9, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, text.encode("utf-8"))
    return out.getvalue()


def employer_letter_docx(corrected: bool = False) -> bytes:
    return docx(facts.letter_paragraphs(corrected), bold={0, 4})


def application_form() -> bytes:
    p = TextPage()
    p.line(facts.AUTHORITY, 15, bold=True)
    p.line(f"Form {facts.FORM_CODE}: application for a {facts.SERVICE}", 12)
    p.para("Fill in every field that applies. Use the formats shown. Sign by typing your name or adding a "
           "signature below the declaration.", 9)
    widgets = []
    y = p.y - 8
    order = [
        "applicant_full_name", "date_of_birth", "id_card_number", "id_card_expiry", "current_address", "town",
        "postcode", "moved_in", "lived_here_three_years", "previous_address", "currently_employed",
        "employer_name", "job_title", "employment_start", "monthly_gross_income", "email", "phone",
        "declaration", "date_signed",
    ]
    for name in order:
        label = facts.FORM_LABELS[name]
        checkbox = name in facts.FORM_CHECKBOXES
        lines = wrap(label, 9, 245)
        for i, line in enumerate(lines):
            p.text(60, y - i * 11, line, 9)
        if checkbox:
            rect = (320.0, y - 3, 333.0, y + 10)
            p.box(320, y - 3, 13, 13)
        else:
            rect = (320.0, y - 5, 535.0, y + 12)
        widgets.append(Widget(name, label, rect, checkbox))
        y -= max(29, 11 * len(lines) + 16)
    p.text(60, y - 4, "Signature", 9, bold=True)
    p.rule(320, y - 6, 215, 0.6)
    footer(p)
    return write_document([p], f"Form {facts.FORM_CODE}", widgets={0: widgets})


def evidence_page(key: str) -> TextPage:
    meta = {e["key"]: e for e in facts.EVIDENCE}[key]
    p = TextPage()
    p.line(meta["title"], 15, bold=True)
    p.space(6)
    if key == "EV1":
        p.para(f"This agreement is made on 20/02/2024 between {facts.LANDLORD} (the landlord) and "
               f"{facts.APPLICANT['full_name']} (the tenant).")
        p.para(f"Property: {facts.CURRENT_ADDRESS['line1']}, {facts.CURRENT_ADDRESS['town']} "
               f"{facts.CURRENT_ADDRESS['postcode']}.")
        p.para(f"The tenancy starts on {facts.CURRENT_ADDRESS['moved_in_long']} and runs from month to month. "
               "The rent is 1,050.00 a month, paid on the first day of each month.")
        p.para("The tenant keeps the property clean and reports repairs promptly. The landlord answers repair "
               "requests within ten working days.")
    elif key == "EV2":
        p.para("Either party may end the tenancy with two months' notice in writing.")
        p.para("Signed for the landlord: Imogen Varga, lettings officer.")
        p.para(f"Signed by the tenant: {facts.APPLICANT['full_name']}.")
        p.para("Date: 20/02/2024")
    elif key == "EV3":
        p.para(f"Council of Northvale. Statement for {facts.APPLICANT['full_name']}, "
               f"{facts.CURRENT_ADDRESS['line1']}, {facts.CURRENT_ADDRESS['postcode']}.")
        p.para("Band C. Annual charge 1,486.00, paid in ten instalments of 148.60 from April 2026.")
        p.para(f"Previous address on our records: {facts.PREVIOUS_ADDRESS['single_line']}, from "
               f"{facts.PREVIOUS_ADDRESS['moved_in']} to {facts.PREVIOUS_ADDRESS['moved_out']}.")
    elif key == "EV4":
        p.para(f"Account holder: {facts.APPLICANT['full_name']}. Statement period: September 2026.")
        for date, text, amount in [
            ("01/09/2026", "Rent, Ardent Lettings Cooperative", "-1,050.00"),
            ("05/09/2026", "Northvale Water and Power Cooperative", "-84.17"),
            ("30/09/2026", "Salary, HALBERD AND FINCH JOINERY", "2,571.40"),
        ]:
            p.text(60, p.y, date, 10)
            p.text(140, p.y, text, 10)
            p.text(450, p.y, amount, 10)
            p.space(15)
    footer(p, f"{meta['marker']}. {FICTION_NOTE}")
    return p


def supporting_evidence() -> bytes:
    pages = []
    for key in facts.SOURCE_EVIDENCE_ORDER:
        if key == "BLANK":
            pages.append(TextPage())
            continue
        page = evidence_page(key)
        if key == facts.UPSIDE_DOWN:
            page.rotate = 180
        pages.append(page)
    return write_document(pages, "Supporting evidence")


def build_documents() -> dict[str, bytes]:
    """Every source document, by its path under the trial's Sources folder."""
    docs = {
        "id-card-scan-a.pdf": id_card_pdf(facts.EXPIRED_ID, seed=7),
        "id-card-scan-b.pdf": id_card_pdf(facts.CURRENT_ID, seed=11),
        "utility-bill.pdf": utility_bill(),
        "employer-letter.docx": employer_letter_docx(),
        "employer-letter.pdf": employer_letter_pdf(),
        f"application-form-{facts.FORM_CODE}.pdf": application_form(),
        "supporting-evidence.pdf": supporting_evidence(),
    }
    for slip in facts.PAYSLIPS:
        docs[slip["file"]] = payslip(slip)
    return dict(sorted(docs.items()))


CURRENT_ID_FILE = "id-card-scan-b.pdf"
EXPIRED_ID_FILE = "id-card-scan-a.pdf"


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: generate.py OUT_DIR", file=sys.stderr)
        return 2
    for path in write_documents(Path(argv[1])):
        print(f"wrote {path}")
    return 0


def write_documents(out: Path) -> list[Path]:
    out.mkdir(parents=True, exist_ok=True)
    written = []
    for name, data in build_documents().items():
        (out / name).write_bytes(data)
        written.append(out / name)
    return written


if __name__ == "__main__":
    sys.exit(main(sys.argv))
