"""The trial pack's generator, portal and checker, end to end, with no desktop.

A simulated agent fills the portal over real HTTP on loopback and writes the
outputs a correct solution would; the checker must pass every task on it and
name the fault in each wrong one.
"""

from __future__ import annotations

import http.client
import json
import re
import shutil
import sys
import tempfile
import threading
import unittest
import urllib.parse
from pathlib import Path

PACK = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACK))

import check  # noqa: E402
import facts  # noqa: E402
import generate  # noqa: E402
import pdfmini  # noqa: E402
import portal  # noqa: E402
from pdfmini import Name, write_document  # noqa: E402


class Client:
    """A browser stand-in: form posts, multipart uploads, 303s followed."""

    def __init__(self, port: int) -> None:
        self.port = port
        self.url = "/"

    def request(self, method: str, path: str, body: bytes = b"", ctype: str = "") -> tuple[int, str]:
        for _ in range(5):
            conn = http.client.HTTPConnection("127.0.0.1", self.port, timeout=10)
            headers = {"Content-Type": ctype} if ctype else {}
            conn.request(method, path, body=body if method == "POST" else None, headers=headers)
            response = conn.getresponse()
            text = response.read().decode("utf-8")
            conn.close()
            if response.status == 303:
                method, path, body = "GET", response.getheader("Location"), b""
                continue
            self.url = path
            return response.status, text
        raise AssertionError("too many redirects")

    def post(self, path: str, fields: dict[str, str]) -> tuple[int, str]:
        return self.request("POST", path, urllib.parse.urlencode(fields).encode(),
                            "application/x-www-form-urlencoded")

    def upload(self, path: str, fields: dict[str, str], files: list[tuple[str, str, bytes]]) -> tuple[int, str]:
        boundary = "trialboundary7c1e"
        out = bytearray()
        for name, value in fields.items():
            out += (f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"\r\n\r\n{value}\r\n').encode()
        for name, filename, data in files:
            out += (f'--{boundary}\r\nContent-Disposition: form-data; name="{name}"; filename="{filename}"\r\n'
                    "Content-Type: application/octet-stream\r\n\r\n").encode() + data + b"\r\n"
        out += f"--{boundary}--\r\n".encode()
        return self.request("POST", path, bytes(out), f"multipart/form-data; boundary={boundary}")

    @property
    def token(self) -> str:
        return self.url.split("/")[2]


def filled_form(blank: bytes, overrides: dict[str, object] | None = None) -> bytes:
    """The blank form with every answer set, saved as an editor does: an
    incremental update rewriting each field."""
    answers: dict[str, object] = {**facts.FORM_TEXT_FIELDS, **facts.FORM_CHECKBOXES, "date_signed": "20/10/2026"}
    answers.update(overrides or {})
    doc = pdfmini.Document(blank)
    updated = {}
    for num, obj in doc.objects.items():
        if isinstance(obj, dict) and "T" in obj:
            name = pdfmini.text_string(obj["T"])
            value = answers.get(name)
            entry = dict(obj)
            if isinstance(value, bool):
                entry["V"] = entry["AS"] = Name("Yes" if value else "Off")
            elif value is not None:
                entry["V"] = str(value)
            for key, item in entry.items():
                if isinstance(item, (pdfmini.LitString, pdfmini.HexString)):
                    entry[key] = pdfmini.text_string(item)
            updated[num] = entry
    return pdfmini.incremental_update(blank, updated)


def pages_pdf(pages: list) -> bytes:
    return write_document(pages, "Assembled")


SLIPS = {s["period"]: s for s in facts.PAYSLIPS}


class Pack(unittest.TestCase):
    def setUp(self) -> None:
        self.home = Path(tempfile.mkdtemp(prefix="trial-home-"))
        self.addCleanup(shutil.rmtree, self.home)
        self.trial = self.home / facts.TRIAL_DIR
        self.sources = self.trial / facts.SOURCES
        self.out = self.trial / facts.OUT
        self.state = self.home / facts.STATE_DIR
        generate.write_documents(self.sources)
        self.out.mkdir()
        (self.trial / facts.SUBMISSION).mkdir()
        self.server = portal.make_server(self.state, 0, quiet=True)
        self.port = self.server.server_address[1]
        thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def results(self) -> dict[str, check.Result]:
        trial = check.Trial(self.home, self.trial, self.state, generate.build_documents())
        return {r.task: r for r in check.run(trial, list(check.TASKS))}

    # -- solutions ---------------------------------------------------------

    def write_documents_solution(self) -> None:
        self.out.joinpath("employer-letter-corrected.docx").write_bytes(generate.employer_letter_docx(corrected=True))
        self.out.joinpath("employer-letter-corrected.pdf").write_bytes(generate.employer_letter_pdf(corrected=True))
        blank = (self.sources / f"application-form-{facts.FORM_CODE}.pdf").read_bytes()
        self.out.joinpath("application-form-filled.pdf").write_bytes(filled_form(blank))
        self.out.joinpath("supporting-evidence-ordered.pdf").write_bytes(
            pages_pdf([generate.evidence_page(k) for k in ("EV1", "EV2", "EV3", "EV4")]))
        self.out.joinpath("income-evidence.pdf").write_bytes(pages_pdf([
            generate.payslip_page(SLIPS["August 2026"]), generate.payslip_page(SLIPS["September 2026"]),
            generate.evidence_page("EV4")]))

    def file_submission(self) -> None:
        sub = self.trial / facts.SUBMISSION
        for name in ("application-form-filled.pdf", "income-evidence.pdf"):
            (self.out / name).rename(sub / name)
        shutil.copy(self.sources / generate.CURRENT_ID_FILE, sub / "identity-card.pdf")
        shutil.copy(self.sources / "utility-bill.pdf", sub / "utility-bill.pdf")

    def portal_until_saved(self, c: Client) -> str:
        status, text = c.request("GET", "/")
        self.assertEqual(status, 200)
        self.assertIn('<label for="f-reference_code">', text)
        c.post("/start", {})
        personal = dict(facts.PORTAL_ANSWERS["personal"])
        # As the card prints it, which the portal refuses, and the expired card.
        status, text = c.post(c.url, {**personal, "date_of_birth": "17 APR 1988",
                                      "id_number": facts.EXPIRED_ID["number"], "id_expiry": facts.EXPIRED_ID["expires"],
                                      "action": "continue"})
        self.assertIn("There is a problem", text)
        self.assertIn("as day, month and year", text)
        self.assertIn("This identity card has expired", text)
        self.assertIn('aria-invalid="true"', text)
        c.post(c.url, {**personal, "action": "continue"})
        self.assertTrue(c.url.endswith("/address"), c.url)
        address = dict(facts.PORTAL_ANSWERS["address"])
        status, text = c.post(c.url, {**address, "lived_three_years": "yes", "previous_postcode": "",
                                      "postcode": "nv47qd", "action": "continue"})
        self.assertIn("with a space before the last three characters", text)
        self.assertIn("You moved in less than three years ago", text)
        status, text = c.post(c.url, {**address, "previous_town": "", "action": "continue"})
        self.assertIn("Enter your previous town, because", text)
        c.post(c.url, {**address, "action": "continue"})
        self.assertTrue(c.url.endswith("/employment"), c.url)
        old = c.url
        status, text = c.post(c.url, {"employment_status": "employed", "employer_name": facts.EMPLOYER["name"],
                                      "action": "save"})
        code = re.search(r'id="reference-code">([^<]+)<', text).group(1)
        self.assertRegex(code, r"^NV-[A-Z0-9]{4}-[A-Z0-9]{4}$")
        status, _ = c.request("GET", old)
        self.assertEqual(status, 410, "a saved session ends; only the reference code resumes it")
        return code

    def portal_resume_and_submit(self, c: Client, code: str, identity: str, income: list[str],
                                 form: bytes) -> str:
        status, text = c.post("/resume", {"reference_code": "NV-ZZZZ-ZZZZ"})
        self.assertIn("We could not find an application", text)
        c.post("/resume", {"reference_code": code.lower().replace("-", "- ")})
        self.assertTrue(c.url.endswith("/employment"), c.url)
        employment = dict(facts.PORTAL_ANSWERS["employment"])
        status, text = c.post(c.url, {**employment, "monthly_gross_income": "3,250.00", "action": "continue"})
        self.assertIn("Leave out commas", text)
        status, text = c.post(c.url, {**employment, "has_other_income": "yes", "action": "continue"})
        self.assertIn("Tell us about your other income", text)
        c.post(c.url, {**employment, "action": "continue"})
        self.assertTrue(c.url.endswith("/documents"), c.url)
        docs = c.url
        letter = (self.sources / "employer-letter.docx").read_bytes()
        status, text = c.upload(docs, {"action": "upload"}, [("income", "employer-letter.docx", letter)])
        self.assertIn("employer-letter.docx must be a PDF", text)
        status, text = c.upload(docs, {"action": "upload"},
                                [("proof_of_address", "huge.pdf", b"%PDF-1.4\n" + b"0" * portal.MAX_UPLOAD)])
        self.assertIn("huge.pdf is larger than 5 MB", text)
        status, text = c.upload(docs, {"action": "continue"}, [])
        self.assertIn("Upload your identity document", text)
        files = [("identity", identity, (self.sources / identity).read_bytes()),
                 ("proof_of_address", "utility-bill.pdf", (self.sources / "utility-bill.pdf").read_bytes())]
        files += [("income", name, (self.sources / name).read_bytes()) for name in income]
        files.append(("signed_form", "application-form-filled.pdf", form))
        status, text = c.upload(docs, {"action": "continue"}, files)
        self.assertTrue(c.url.endswith("/review"), (c.url, text[:2000]))
        status, text = c.post(c.url, {"action": "submit"})
        self.assertIn("Confirm the declaration", text)
        status, text = c.post(c.url, {"declaration": "yes", "action": "submit"})
        return re.search(r'id="confirmation-number">([^<]+)<', text).group(1)

    # -- tests -------------------------------------------------------------

    def test_documents_are_reproducible_and_readable(self) -> None:
        docs = generate.build_documents()
        self.assertEqual(docs, generate.build_documents())
        for name, data in docs.items():
            self.assertEqual((self.sources / name).read_bytes(), data, name)
        evidence = pdfmini.Document(docs["supporting-evidence.pdf"])
        kinds = [check.page_kind(p) for p in evidence.pages()]
        self.assertEqual(kinds, ["EV3", "EV1", "blank page", "EV4", "EV2"])
        self.assertEqual([p.rotate for p in evidence.pages()], [0, 0, 0, 0, 180])
        scan = pdfmini.Document(docs[generate.CURRENT_ID_FILE]).pages()[0]
        self.assertEqual(scan.text(), "", "an identity scan is pixels only")
        self.assertTrue(scan.has_images())
        fields = pdfmini.Document(docs[f"application-form-{facts.FORM_CODE}.pdf"]).fields()
        self.assertEqual(set(fields), set(facts.FORM_LABELS))
        for name, f in fields.items():
            if name in facts.FORM_CHECKBOXES:
                self.assertEqual((f.kind, f.checked), ("Btn", False), name)
            else:
                self.assertEqual((f.kind, f.text), ("Tx", ""), name)
        letter = check.document_text(self.sources / "employer-letter.docx")
        for typo in facts.LETTER_TYPOS:
            self.assertIn(typo, letter)

    def test_an_untouched_trial_fails_every_task_with_a_reason(self) -> None:
        for name, result in self.results().items():
            self.assertFalse(result.passed, name)
            self.assertNotIn("could not observe", result.reason, f"{name}: {result.reason}")

    def test_a_correct_solution_passes_every_task(self) -> None:
        self.write_documents_solution()
        c = Client(self.port)
        code = self.portal_until_saved(c)
        self.out.joinpath("reference-code.txt").write_text(code + "\n")
        partial = self.results()
        self.assertTrue(partial["6-portal-save"].passed, partial["6-portal-save"].reason)
        self.assertFalse(partial["7-portal-submit"].passed)
        form = (self.out / "application-form-filled.pdf").read_bytes()
        confirmation = self.portal_resume_and_submit(
            c, code, generate.CURRENT_ID_FILE, ["payslip-3.pdf", "payslip-1.pdf"], form)
        self.out.joinpath("confirmation.txt").write_text(f"Confirmation: {confirmation}\n")
        self.file_submission()
        results = self.results()
        for name, result in results.items():
            self.assertTrue(result.passed, f"{name}: {result.reason}")
        app = json.loads(next((self.state / "applications").glob("*.json")).read_text())
        self.assertEqual(app["status"], "submitted")
        self.assertEqual({u["category"] for u in app["uploads"]},
                         {"identity", "proof_of_address", "income", "signed_form"})
        events = [e["type"] for e in app["events"]]
        for kind in ("saved", "resumed", "validation_failed", "upload_rejected", "submitted"):
            self.assertIn(kind, events)
        self.assertRegex(results["7-portal-submit"].reason, r"\(\d+ validation errors and 2 refused uploads")

    def test_wrong_solutions_fail_naming_the_fault(self) -> None:
        self.write_documents_solution()
        # A typo left in, the blank page kept, the payslips in the wrong order,
        # a box ticked that must be clear.
        paragraphs = facts.letter_paragraphs(corrected=True)
        paragraphs[6] = paragraphs[6].replace("receives", "recieves")
        self.out.joinpath("employer-letter-corrected.docx").write_bytes(generate.docx(paragraphs, set()))
        self.out.joinpath("supporting-evidence-ordered.pdf").write_bytes(pages_pdf(
            [generate.evidence_page("EV1"), generate.evidence_page("EV2"), pdfmini.TextPage(),
             generate.evidence_page("EV3"), generate.evidence_page("EV4")]))
        self.out.joinpath("income-evidence.pdf").write_bytes(pages_pdf([
            generate.payslip_page(SLIPS["September 2026"]), generate.payslip_page(SLIPS["August 2026"]),
            generate.evidence_page("EV4")]))
        blank = (self.sources / f"application-form-{facts.FORM_CODE}.pdf").read_bytes()
        wrong_form = filled_form(blank, {"lived_here_three_years": True, "postcode": "NV4 7QF"})
        self.out.joinpath("application-form-filled.pdf").write_bytes(wrong_form)
        c = Client(self.port)
        code = self.portal_until_saved(c)
        self.out.joinpath("reference-code.txt").write_text(code)
        confirmation = self.portal_resume_and_submit(
            c, code, generate.EXPIRED_ID_FILE, ["payslip-2.pdf", "payslip-1.pdf"], wrong_form)
        self.out.joinpath("confirmation.txt").write_text(confirmation)
        results = self.results()
        self.assertIn("still says 'recieves'", results["1-letter"].reason)
        self.assertIn("blank page", results["3-evidence"].reason)
        self.assertIn("expected [1: payslip August 2026", results["4-combine"].reason)
        self.assertIn("lived_here_three_years should be clear", results["2-form"].reason)
        self.assertIn("postcode is 'NV4 7QF'", results["2-form"].reason)
        self.assertTrue(results["6-portal-save"].passed, results["6-portal-save"].reason)
        reason = results["7-portal-submit"].reason
        self.assertIn("the expired card was uploaded", reason)
        self.assertIn("the September 2025 payslip is a year old", reason)
        self.assertIn("signed form:", reason)
        self.assertFalse(results["5-filing"].passed)
        for name in ("1-letter", "2-form", "3-evidence", "4-combine", "7-portal-submit"):
            self.assertFalse(results[name].passed, name)

    def test_the_portal_binds_loopback_only(self) -> None:
        with self.assertRaises(SystemExit):
            portal.make_server(self.state, 0, host="0.0.0.0")


if __name__ == "__main__":
    unittest.main()
