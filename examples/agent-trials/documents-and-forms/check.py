#!/usr/bin/env python3
"""Check each trial task's outcome from the files and the portal's state.

    python3 check.py [--home DIR] [--json] [TASK ...]

Prints PASS or FAIL per task with the reason, and exits non-zero when any
checked task fails. A task whose subject cannot be observed (a missing file,
an unreadable PDF, no portal state) fails and says why; nothing is skipped.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
import zipfile
from dataclasses import dataclass
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Callable
from xml.etree import ElementTree

import facts
import generate
import pdfmini
import portal
from pdfmini import squash

# ----------------------------------------------------------------------------
# Results
# ----------------------------------------------------------------------------


@dataclass
class Result:
    task: str
    passed: bool
    reason: str


class Fail(Exception):
    """The task fails, for this reason."""


@dataclass
class Trial:
    home: Path
    trial: Path
    state: Path
    originals: dict[str, bytes]
    # What the agent reported, when the operator records it instead of a file.
    reference_code: str | None = None
    confirmation: str | None = None

    @property
    def sources(self) -> Path:
        return self.trial / facts.SOURCES

    @property
    def out(self) -> Path:
        return self.trial / facts.OUT

    @property
    def submission(self) -> Path:
        return self.trial / facts.SUBMISSION

    def sha(self, name: str) -> str:
        return hashlib.sha256(self.originals[name]).hexdigest()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def norm(value: str) -> str:
    return re.sub(r"\s+", " ", str(value)).strip().casefold()


def same(field_name: str, got: str, want: str) -> bool:
    if "postcode" in field_name or field_name == "phone":
        return norm(got).replace(" ", "") == norm(want).replace(" ", "")
    if field_name == "monthly_gross_income":
        try:
            return Decimal(got.strip()) == Decimal(want)
        except InvalidOperation:
            return False
    return norm(got) == norm(want)


# ----------------------------------------------------------------------------
# Reading what the agent made
# ----------------------------------------------------------------------------


def open_pdf(path: Path) -> pdfmini.Document:
    if not path.is_file():
        raise Fail(f"{path} does not exist")
    try:
        return pdfmini.Document(path.read_bytes())
    except (pdfmini.PdfError, ValueError, KeyError, IndexError, TypeError) as error:
        raise Fail(f"{path} cannot be read as a PDF: {error}") from error


def find_output(trial: Trial, name: str) -> Path:
    """An output the agent may have left in Out or already filed in Submission."""
    for folder in (trial.out, trial.submission):
        if (folder / name).is_file():
            return folder / name
    raise Fail(f"{name} is in neither {trial.out} nor {trial.submission}")


PAGE_SIGNATURES = {
    **{f"payslip {s['period']}": squash(f"Pay period: {s['period']}") for s in facts.PAYSLIPS},
    **{e["key"]: squash(e["marker"]) for e in facts.EVIDENCE},
}
EVIDENCE_NAMES = {e["key"]: e["title"] for e in facts.EVIDENCE}


def page_kind(page: pdfmini.Page) -> str:
    try:
        text = squash(page.text())
    except (pdfmini.PdfError, ValueError, KeyError, IndexError, TypeError) as error:
        raise Fail(f"a page's text cannot be read: {error}") from error
    found = [key for key, sig in PAGE_SIGNATURES.items() if sig in text]
    if len(found) == 1:
        return found[0]
    if not text and not page.has_images():
        return "blank page"
    if found:
        return " + ".join(found)
    return "unknown page"


def describe(kinds: list[str]) -> str:
    return ", ".join(f"{i + 1}: {EVIDENCE_NAMES.get(k, k)}" for i, k in enumerate(kinds))


def check_pages(path: Path, want: list[str]) -> str:
    doc = open_pdf(path)
    pages = doc.pages()
    kinds = [page_kind(p) for p in pages]
    if kinds != want:
        raise Fail(f"{path.name} has pages [{describe(kinds)}]; expected [{describe(want)}]")
    turned = [i + 1 for i, p in enumerate(pages) if p.rotate != 0]
    if turned:
        raise Fail(f"{path.name} shows page {', '.join(map(str, turned))} rotated; every page must be upright")
    return f"{path.name}: {len(pages)} pages in order, all upright"


def check_form_fields(path: Path) -> str:
    doc = open_pdf(path)
    fields = doc.fields()
    if not fields:
        raise Fail(f"{path.name} has no form fields left (flattened?); keep the fields so the answers can "
                   "be checked")
    problems = []
    for name, want in facts.FORM_TEXT_FIELDS.items():
        if name not in fields:
            problems.append(f"field {name} is missing")
        elif not same(name, fields[name].text, want):
            problems.append(f"{name} is {fields[name].text!r}, expected {want!r}")
    for name, want in facts.FORM_CHECKBOXES.items():
        if name not in fields:
            problems.append(f"checkbox {name} is missing")
        elif fields[name].checked != want:
            problems.append(f"{name} should be {'ticked' if want else 'clear'}")
    signed = fields.get("date_signed")
    if signed is None or not portal.DATE.match(signed.text.strip()):
        problems.append("date_signed is not a date as DD/MM/YYYY")
    if problems:
        raise Fail(f"{path.name}: " + "; ".join(problems[:6]) + (" ..." if len(problems) > 6 else ""))
    return f"{path.name}: all {len(facts.FORM_TEXT_FIELDS)} answers and {len(facts.FORM_CHECKBOXES)} boxes right"


def document_text(path: Path) -> str:
    """The paragraphs of a .docx or .odt, one per line."""
    try:
        with zipfile.ZipFile(path) as archive:
            if path.suffix == ".docx":
                root = ElementTree.fromstring(archive.read("word/document.xml"))
                ns = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
                paragraphs = ["".join(t.text or "" for t in p.iter(f"{ns}t")) for p in root.iter(f"{ns}p")]
            else:
                root = ElementTree.fromstring(archive.read("content.xml"))
                ns = "{urn:oasis:names:tc:opendocument:xmlns:text:1.0}"
                paragraphs = ["".join(p.itertext()) for p in root.iter() if p.tag in (f"{ns}p", f"{ns}h")]
    except (zipfile.BadZipFile, KeyError, ElementTree.ParseError) as error:
        raise Fail(f"{path.name} cannot be read as a {path.suffix} document: {error}") from error
    return "\n".join(paragraphs)


def letter_problems(text: str, where: str) -> list[str]:
    if not squash(text):
        return [f"no text could be read from {where}"]
    problems = []
    for typo in facts.LETTER_TYPOS:
        if re.search(rf"\b{re.escape(typo)}\b", text, re.IGNORECASE):
            problems.append(f"{where} still says {typo!r}")
    if squash(facts.EMPLOYER["wrong_start_long"]) in squash(text):
        problems.append(f"{where} still gives the start date as {facts.EMPLOYER['wrong_start_long']}")
    flat = squash(text)
    for paragraph in facts.letter_paragraphs(corrected=True):
        if squash(paragraph) not in flat:
            problems.append(f"{where} lacks the paragraph {paragraph[:60]!r}...")
    return problems


# ----------------------------------------------------------------------------
# Tasks
# ----------------------------------------------------------------------------


def task_letter(t: Trial) -> str:
    docs = [t.out / f"employer-letter-corrected{ext}" for ext in (".docx", ".odt")]
    doc = next((d for d in docs if d.is_file()), None)
    if doc is None:
        raise Fail(f"neither {docs[0]} nor {docs[1]} exists")
    problems = letter_problems(document_text(doc), doc.name)
    pdf_path = t.out / "employer-letter-corrected.pdf"
    pdf = open_pdf(pdf_path)
    try:
        pdf_text = "\n".join(p.text() for p in pdf.pages())
    except (pdfmini.PdfError, ValueError, KeyError, IndexError, TypeError) as error:
        raise Fail(f"{pdf_path.name}: its text cannot be read: {error}") from error
    problems += letter_problems(pdf_text, pdf_path.name)
    if problems:
        raise Fail("; ".join(problems[:5]))
    return f"{doc.name} and {pdf_path.name} are corrected and keep the rest of the letter"


def task_form(t: Trial) -> str:
    return check_form_fields(find_output(t, "application-form-filled.pdf"))


EVIDENCE_ORDER = [e["key"] for e in facts.EVIDENCE]
PAYSLIPS = {s["period"]: s["file"] for s in facts.PAYSLIPS}
COMBINED_ORDER = ["payslip August 2026", "payslip September 2026", "EV4"]


def task_evidence(t: Trial) -> str:
    return check_pages(find_output(t, "supporting-evidence-ordered.pdf"), EVIDENCE_ORDER)


def task_combine(t: Trial) -> str:
    return check_pages(find_output(t, "income-evidence.pdf"), COMBINED_ORDER)


def task_filing(t: Trial) -> str:
    sub = t.submission
    if not sub.is_dir():
        raise Fail(f"{sub} does not exist")
    if not t.sources.is_dir():
        raise Fail(f"{t.sources} does not exist, so the copies cannot be told from moves")
    problems = []
    moved = ["application-form-filled.pdf", "income-evidence.pdf"]
    for name in moved:
        if not (sub / name).is_file():
            problems.append(f"{name} is not in {sub.name}")
        if (t.out / name).exists():
            problems.append(f"{name} is still in {t.out.name}; it should be moved, not copied")
    files = sorted(p for p in sub.iterdir() if p.is_file() and not p.name.startswith("."))
    hashes = {p.name: sha256(p) for p in files}
    current, expired, bill = t.sha(generate.CURRENT_ID_FILE), t.sha(generate.EXPIRED_ID_FILE), t.sha("utility-bill.pdf")
    if current not in hashes.values():
        problems.append("the current identity card scan is not in Submission")
    if expired in hashes.values():
        problems.append("the expired identity card scan is in Submission")
    if bill not in hashes.values():
        problems.append("the utility bill is not in Submission")
    for original in (generate.CURRENT_ID_FILE, "utility-bill.pdf"):
        if not (t.sources / original).is_file() or sha256(t.sources / original) != t.sha(original):
            problems.append(f"{original} is gone from Sources or changed; it should be copied, not moved")
    extra = [n for n, h in hashes.items() if n not in moved and h not in (current, bill)]
    if extra:
        problems.append(f"Submission holds files that do not belong: {', '.join(extra)}")
    if len(files) != 4 and not extra:
        problems.append(f"Submission holds {len(files)} files, expected 4")
    if problems:
        raise Fail("; ".join(problems))
    notes = [check_form_fields(sub / "application-form-filled.pdf"), check_pages(sub / "income-evidence.pdf",
                                                                                 COMBINED_ORDER)]
    return "Submission holds exactly the four right files. " + " ".join(notes)


CODE = re.compile(r"NV-[A-Z0-9]{4}-[A-Z0-9]{4}")


def portal_app(t: Trial) -> dict:
    ref = t.out / "reference-code.txt"
    if t.reference_code is not None:
        reported, where = t.reference_code, "the reported reference code"
    elif ref.is_file():
        reported, where = ref.read_text("utf-8", errors="replace"), ref.name
    else:
        raise Fail(f"{ref} does not exist and no --reference-code was given")
    match = CODE.search(reported.upper())
    if not match:
        raise Fail(f"{where} holds no reference code like NV-ABCD-2345")
    apps_dir = t.state / "applications"
    if not apps_dir.is_dir():
        raise Fail(f"the portal's state ({apps_dir}) does not exist; is the portal installed and running?")
    for path in sorted(apps_dir.glob("*.json")):
        app = json.loads(path.read_text("utf-8"))
        if app.get("reference_code") == match.group(0):
            return app
    raise Fail(f"no application in the portal has the reference code {match.group(0)}")


def answer_problems(app: dict, steps: list[str]) -> list[str]:
    problems = []
    for step in steps:
        answers = app["answers"].get(step, {})
        errors = portal.validate(step, answers)
        if errors:
            problems.append(f"{step}: the portal would refuse it ({next(iter(errors.values()))})")
            continue
        for name, want in facts.PORTAL_ANSWERS[step].items():
            if not same(name, answers.get(name, ""), want):
                problems.append(f"{step}: {name} is {answers.get(name, '')!r}, expected {want!r}")
    return problems


def task_portal_save(t: Trial) -> str:
    app = portal_app(t)
    if not any(e["type"] == "saved" for e in app["events"]):
        raise Fail(f"application {app['reference_code']} was never saved for later")
    problems = answer_problems(app, ["personal", "address"])
    if problems:
        raise Fail("; ".join(problems[:5]))
    return f"application {app['reference_code']} is saved with the right details and address"


def task_portal_submit(t: Trial) -> str:
    app = portal_app(t)
    code = app["reference_code"]
    if app["status"] != "submitted":
        raise Fail(f"application {code} is not submitted")
    types = [e["type"] for e in app["events"]]
    if "saved" not in types or "resumed" not in types[types.index("saved"):]:
        raise Fail(f"application {code} was never resumed from its reference code after being saved")
    conf = t.out / "confirmation.txt"
    if t.confirmation is not None:
        reported, where = t.confirmation, "the reported confirmation number"
    elif conf.is_file():
        reported, where = conf.read_text("utf-8", errors="replace"), conf.name
    else:
        raise Fail(f"{conf} does not exist and no --confirmation was given")
    if app["confirmation_number"] not in reported.upper():
        raise Fail(f"{where} is not the confirmation number {app['confirmation_number']}")
    problems = answer_problems(app, ["personal", "address", "employment"])
    uploads: dict[str, list[dict]] = {key: [] for key in portal.CATEGORIES}
    for upload in app["uploads"]:
        stored = Path(upload["path"])
        if not stored.is_file() or sha256(stored) != upload["sha256"]:
            raise Fail(f"the uploaded file {upload['filename']} is missing from the portal's state")
        uploads[upload["category"]].append(upload)
    shas = {key: sorted(u["sha256"] for u in items) for key, items in uploads.items()}
    if shas["identity"] != [t.sha(generate.CURRENT_ID_FILE)]:
        expired = t.sha(generate.EXPIRED_ID_FILE) in shas["identity"]
        problems.append("identity: " + ("the expired card was uploaded" if expired
                                        else "it must hold exactly the current identity card scan"))
    if shas["proof_of_address"] != [t.sha("utility-bill.pdf")]:
        problems.append("proof of address: it must hold exactly the utility bill")
    want_income = sorted(t.sha(PAYSLIPS[p]) for p in ("August 2026", "September 2026"))
    if shas["income"] != want_income:
        stale = t.sha(PAYSLIPS["September 2025"]) in shas["income"]
        problems.append("income: " + ("the September 2025 payslip is a year old" if stale
                                      else "it must hold exactly the August and September 2026 payslips"))
    if len(uploads["signed_form"]) != 1:
        problems.append(f"signed form: {len(uploads['signed_form'])} files uploaded, expected 1")
    else:
        try:
            check_form_fields(Path(uploads["signed_form"][0]["path"]))
        except Fail as error:
            problems.append(f"signed form: {error}")
    if problems:
        raise Fail("; ".join(problems[:6]))
    failures = sum(1 for e in app["events"] if e["type"] == "validation_failed")
    rejected = sum(1 for e in app["events"] if e["type"] == "upload_rejected")
    return (f"application {code} submitted as {app['confirmation_number']} with the right answers and documents "
            f"({failures} validation errors and {rejected} refused uploads on the way)")


TASKS: dict[str, Callable[[Trial], str]] = {
    "1-letter": task_letter,
    "2-form": task_form,
    "3-evidence": task_evidence,
    "4-combine": task_combine,
    "5-filing": task_filing,
    "6-portal-save": task_portal_save,
    "7-portal-submit": task_portal_submit,
}


def run(trial: Trial, names: list[str]) -> list[Result]:
    results = []
    for name in names:
        try:
            results.append(Result(name, True, TASKS[name](trial)))
        except Fail as error:
            results.append(Result(name, False, str(error)))
        except Exception as error:  # noqa: BLE001 - a checker bug must still report, not skip
            results.append(Result(name, False, f"the checker could not observe this task: "
                                               f"{type(error).__name__}: {error}"))
    return results


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("tasks", nargs="*", help=f"tasks to check (default all): {', '.join(TASKS)}")
    parser.add_argument("--home", type=Path, default=Path.home())
    parser.add_argument("--trial-dir", type=Path, help=f"default HOME/{facts.TRIAL_DIR}")
    parser.add_argument("--state-dir", type=Path, help=f"default HOME/{facts.STATE_DIR}")
    parser.add_argument("--reference-code", help="the code the agent reported, instead of Out/reference-code.txt")
    parser.add_argument("--confirmation", help="the number the agent reported, instead of Out/confirmation.txt")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    names = []
    for name in args.tasks or list(TASKS):
        matches = [t for t in TASKS if t == name or t.split("-", 1)[0] == name]
        if not matches:
            parser.error(f"unknown task {name}")
        names += matches
    trial = Trial(
        home=args.home,
        trial=args.trial_dir or args.home / facts.TRIAL_DIR,
        state=args.state_dir or args.home / facts.STATE_DIR,
        originals=generate.build_documents(),
        reference_code=args.reference_code,
        confirmation=args.confirmation,
    )
    results = run(trial, names)
    if args.json:
        print(json.dumps([r.__dict__ for r in results], indent=2))
    else:
        for r in results:
            print(f"{'PASS' if r.passed else 'FAIL'} {r.task}: {r.reason}")
        print(f"{sum(r.passed for r in results)} of {len(results)} tasks pass")
    return 0 if all(r.passed for r in results) else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
