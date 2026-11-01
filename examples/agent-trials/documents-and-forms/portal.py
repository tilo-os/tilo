#!/usr/bin/env python3
"""A mock application portal for a fictional permit office, for agent trials.

    python3 portal.py --state-dir DIR [--port 8471]

Standard library only. It binds 127.0.0.1 and nothing else. Every application
is one JSON file under DIR/applications, uploads sit under DIR/uploads, and
each step's answers, every validation failure, save, resume, upload and the
submission are recorded there for check.py to read.

The service is fictional: the Municipal Permit Office of Northvale does not
exist and the pages imitate no real authority's design.
"""

from __future__ import annotations

import argparse
import datetime as dt
import email.parser
import email.policy
import hashlib
import html
import json
import os
import re
import secrets
import sys
import threading
import urllib.parse
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import facts

STEPS = ["personal", "address", "employment", "documents", "review"]
STEP_TITLES = {
    "personal": "Your details",
    "address": "Address history",
    "employment": "Employment and income",
    "documents": "Upload documents",
    "review": "Check your answers",
}
CODE_ALPHABET = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
MAX_UPLOAD = 5 * 1024 * 1024
MAX_REQUEST = 30 * 1024 * 1024
MAX_FORM = 256 * 1024

CATEGORIES = {
    "identity": {
        "label": "Identity document",
        "hint": "Your current, valid identity card. PDF, PNG or JPEG, up to 5 MB.",
        "types": ("pdf", "png", "jpeg"),
        "accept": ".pdf,.png,.jpg,.jpeg,application/pdf,image/png,image/jpeg",
        "minimum": 1,
        "missing": "Upload your identity document",
    },
    "proof_of_address": {
        "label": "Proof of address",
        "hint": "A utility bill or council statement from the last three months, showing your current "
                "address. PDF, up to 5 MB.",
        "types": ("pdf",),
        "accept": ".pdf,application/pdf",
        "minimum": 1,
        "missing": "Upload a proof of address",
    },
    "income": {
        "label": "Proof of income",
        "hint": "Your two most recent monthly payslips. PDF, up to 5 MB each.",
        "types": ("pdf",),
        "accept": ".pdf,application/pdf",
        "minimum": 2,
        "missing": "Upload your two most recent payslips",
    },
    "signed_form": {
        "label": f"Signed application form {facts.FORM_CODE}",
        "hint": f"Form {facts.FORM_CODE}, filled in and signed. PDF, up to 5 MB.",
        "types": ("pdf",),
        "accept": ".pdf,application/pdf",
        "minimum": 1,
        "missing": f"Upload your signed form {facts.FORM_CODE}",
    },
}


def today() -> dt.date:
    override = os.environ.get("TRIAL_PORTAL_TODAY")
    if override:
        return dt.date.fromisoformat(override)
    return dt.date(*facts.TRIAL_TODAY)


def now_iso() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat()


# ----------------------------------------------------------------------------
# Fields and validation
# ----------------------------------------------------------------------------


@dataclass
class Field:
    name: str
    label: str
    kind: str = "text"  # text, email, tel, checkbox, radio
    hint: str = ""
    required: bool = False  # always required, so the page says so too
    options: list[tuple[str, str]] = field(default_factory=list)
    autocomplete: str = ""
    width: str = "wide"


FIELDS: dict[str, list[Field]] = {
    "personal": [
        Field("first_name", "First name", required=True, autocomplete="given-name"),
        Field("last_name", "Last name", required=True, autocomplete="family-name"),
        Field("date_of_birth", "Date of birth", hint="For example, 27/03/1985", required=True,
              autocomplete="bday", width="short"),
        Field("id_number", "Identity card number", hint="For example, NVC-1234-5678", required=True,
              width="short"),
        Field("id_expiry", "Identity card expiry date", hint="For example, 30/06/2031", required=True,
              width="short"),
        Field("email", "Email address", kind="email", required=True, autocomplete="email"),
        Field("phone", "Phone number", kind="tel", hint="For example, 07700 900123", required=True,
              autocomplete="tel", width="short"),
    ],
    "address": [
        Field("address_line1", "Address line 1", required=True, autocomplete="address-line1"),
        Field("town", "Town", required=True, autocomplete="address-level2"),
        Field("postcode", "Postcode", hint="A Northvale postcode, for example NV4 7QD", required=True,
              autocomplete="postal-code", width="short"),
        Field("moved_in", "When did you move in?", hint="Month and year, for example 06/2021", required=True,
              width="short"),
        Field("lived_three_years", "I have lived at this address for three years or more", kind="checkbox"),
        Field("previous_address_line1", "Previous address line 1",
              hint="Only if you have lived at your current address for less than three years"),
        Field("previous_town", "Previous town"),
        Field("previous_postcode", "Previous postcode", hint="For example, EM2 5HB", width="short"),
        Field("previous_moved_in", "When did you move in to your previous address?",
              hint="Month and year, for example 06/2017", width="short"),
        Field("previous_moved_out", "When did you move out of your previous address?",
              hint="Month and year, for example 05/2021", width="short"),
    ],
    "employment": [
        Field("employment_status", "What is your employment status?", kind="radio", required=True,
              options=[("employed", "Employed"), ("self_employed", "Self-employed"),
                       ("not_working", "Not working")]),
        Field("employer_name", "Employer or business name", hint="Only if you are employed or self-employed"),
        Field("job_title", "Job title"),
        Field("employment_start", "When did you start this work?", hint="For example, 01/09/2018",
              width="short"),
        Field("monthly_gross_income", "Monthly income before deductions",
              hint="Numbers only, for example 2500.00", width="short"),
        Field("has_other_income", "I have other income, such as benefits or rent from a lodger", kind="checkbox"),
        Field("other_income_details", "Tell us about your other income",
              hint="What it is and how much you receive each month"),
    ],
    "review": [
        Field("declaration", "I declare that the information I have given is true and complete", kind="checkbox"),
    ],
}

DATE = re.compile(r"^(\d{2})/(\d{2})/(\d{4})$")
MONTH = re.compile(r"^(\d{2})/(\d{4})$")
NORTHVALE_POSTCODE = re.compile(r"^NV\d{1,2} \d[A-Z]{2}$")
ANY_POSTCODE = re.compile(r"^[A-Z]{1,2}\d{1,2} \d[A-Z]{2}$")
ID_NUMBER = re.compile(r"^NVC-\d{4}-\d{4}$")
EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[^@\s]+$")
MONEY = re.compile(r"^\d+(\.\d{2})?$")


def norm_postcode(value: str) -> str:
    return re.sub(r"\s+", " ", value.strip().upper())


def parse_date(value: str) -> dt.date | None:
    m = DATE.match(value.strip())
    if not m:
        return None
    try:
        return dt.date(int(m.group(3)), int(m.group(2)), int(m.group(1)))
    except ValueError:
        return None


def parse_month(value: str) -> dt.date | None:
    m = MONTH.match(value.strip())
    if not m or not 1 <= int(m.group(1)) <= 12:
        return None
    return dt.date(int(m.group(2)), int(m.group(1)), 1)


def years_before(day: dt.date, years: int) -> dt.date:
    try:
        return day.replace(year=day.year - years)
    except ValueError:
        return day.replace(year=day.year - years, day=28)


def date_error(value: str, what: str) -> str | None:
    if not value:
        return f"Enter {what}"
    if parse_date(value) is None:
        return f"Enter {what} as day, month and year, like 27/03/1985"
    return None


def month_error(value: str, what: str) -> str | None:
    if not value:
        return f"Enter {what}"
    if parse_month(value) is None:
        return f"Enter {what} as month and year, like 06/2021"
    if parse_month(value) > today():
        return f"{what[0].upper()}{what[1:]} must be in the past"
    return None


def validate(step: str, a: dict[str, str]) -> dict[str, str]:
    """Errors for one step's answers, by field name, in page order."""
    errors: dict[str, str] = {}
    t = today()
    if step == "personal":
        for name, what in [("first_name", "your first name"), ("last_name", "your last name")]:
            if not a.get(name):
                errors[name] = f"Enter {what}"
        dob = a.get("date_of_birth", "")
        if (e := date_error(dob, "your date of birth")):
            errors["date_of_birth"] = e
        elif parse_date(dob) > years_before(t, 18):
            errors["date_of_birth"] = "You must be 18 or over to apply"
        number = a.get("id_number", "").strip().upper()
        if not number:
            errors["id_number"] = "Enter your identity card number"
        elif not ID_NUMBER.match(number):
            errors["id_number"] = "Enter the identity card number as it is on the card, like NVC-1234-5678"
        expiry = a.get("id_expiry", "")
        if (e := date_error(expiry, "the identity card's expiry date")):
            errors["id_expiry"] = e
        elif parse_date(expiry) <= t:
            errors["id_expiry"] = "This identity card has expired. Use a card that is still valid"
        if not a.get("email"):
            errors["email"] = "Enter your email address"
        elif not EMAIL.match(a["email"]):
            errors["email"] = "Enter an email address like name@example.net"
        digits = re.sub(r"\s", "", a.get("phone", ""))
        if not digits:
            errors["phone"] = "Enter your phone number"
        elif not re.fullmatch(r"0\d{9,12}", digits):
            errors["phone"] = "Enter a phone number like 07700 900123"
    elif step == "address":
        for name, what in [("address_line1", "the first line of your address"), ("town", "your town")]:
            if not a.get(name):
                errors[name] = f"Enter {what}"
        postcode = norm_postcode(a.get("postcode", ""))
        if not postcode:
            errors["postcode"] = "Enter your postcode"
        elif not NORTHVALE_POSTCODE.match(postcode):
            errors["postcode"] = ("Enter a Northvale postcode with a space before the last three characters, "
                                  "like NV4 7QD")
        moved_in = a.get("moved_in", "")
        if (e := month_error(moved_in, "the month you moved in")):
            errors["moved_in"] = e
        long_enough = a.get("lived_three_years") == "yes"
        moved = parse_month(moved_in)
        if long_enough and moved and moved > years_before(t, 3):
            errors["lived_three_years"] = ("You moved in less than three years ago. Clear this box and give "
                                           "your previous address")
        if not long_enough:
            for name, what in [("previous_address_line1", "the first line of your previous address"),
                               ("previous_town", "your previous town")]:
                if not a.get(name):
                    errors[name] = f"Enter {what}, because you have lived at your current address for less " \
                                   "than three years"
            previous = norm_postcode(a.get("previous_postcode", ""))
            if not previous:
                errors["previous_postcode"] = "Enter your previous postcode"
            elif not ANY_POSTCODE.match(previous):
                errors["previous_postcode"] = "Enter a postcode like EM2 5HB"
            for name, what in [("previous_moved_in", "the month you moved in to your previous address"),
                               ("previous_moved_out", "the month you moved out of your previous address")]:
                if (e := month_error(a.get(name, ""), what)):
                    errors[name] = e
            pin, pout = parse_month(a.get("previous_moved_in", "")), parse_month(a.get("previous_moved_out", ""))
            if pin and pout and pout < pin and "previous_moved_out" not in errors:
                errors["previous_moved_out"] = "The month you moved out must be after the month you moved in"
            if pout and moved and pout > moved and "previous_moved_out" not in errors:
                errors["previous_moved_out"] = ("The month you moved out of your previous address must be on or "
                                                "before the month you moved in to your current one")
    elif step == "employment":
        status = a.get("employment_status", "")
        if status not in ("employed", "self_employed", "not_working"):
            errors["employment_status"] = "Choose your employment status"
        if status in ("employed", "self_employed"):
            reason = "because you are employed or self-employed"
            for name, what in [("employer_name", "your employer or business name"), ("job_title", "your job title")]:
                if not a.get(name):
                    errors[name] = f"Enter {what}, {reason}"
            start = a.get("employment_start", "")
            if (e := date_error(start, "the date you started this work")):
                errors["employment_start"] = e
            elif parse_date(start) > t:
                errors["employment_start"] = "The date you started must be in the past"
            income = a.get("monthly_gross_income", "").strip()
            if not income:
                errors["monthly_gross_income"] = f"Enter your monthly income before deductions, {reason}"
            elif not MONEY.match(income):
                errors["monthly_gross_income"] = ("Enter the amount in numbers only, like 2500.00. Leave out "
                                                  "commas and currency signs")
        if a.get("has_other_income") == "yes" and not a.get("other_income_details"):
            errors["other_income_details"] = "Tell us about your other income, because you said you have some"
    elif step == "review":
        if a.get("declaration") != "yes":
            errors["declaration"] = "Confirm the declaration to submit your application"
    return errors


def upload_problems(uploads: list[dict]) -> dict[str, str]:
    problems = {}
    for key, cat in CATEGORIES.items():
        if sum(1 for u in uploads if u["category"] == key) < cat["minimum"]:
            problems[key] = cat["missing"]
    return problems


def sniff(data: bytes) -> str | None:
    if data.startswith(b"%PDF-"):
        return "pdf"
    if data.startswith(b"\x89PNG\r\n\x1a\n"):
        return "png"
    if data.startswith(b"\xff\xd8\xff"):
        return "jpeg"
    return None


EXTENSIONS = {"pdf": ("pdf",), "png": ("png",), "jpeg": ("jpg", "jpeg")}


def check_upload(category: str, filename: str, data: bytes) -> str | None:
    cat = CATEGORIES[category]
    allowed = " or ".join(t.upper() for t in cat["types"])
    ext = filename.rsplit(".", 1)[-1].lower() if "." in filename else ""
    if not data:
        return f"{filename} is empty"
    if len(data) > MAX_UPLOAD:
        return f"{filename} is larger than 5 MB. Upload a smaller file"
    kind = sniff(data)
    if kind not in cat["types"] or ext not in EXTENSIONS.get(kind, ()):
        return f"{filename} must be a {allowed}"
    return None


# ----------------------------------------------------------------------------
# State
# ----------------------------------------------------------------------------


class Store:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.apps = root / "applications"
        self.uploads = root / "uploads"
        self.apps.mkdir(parents=True, exist_ok=True)
        self.uploads.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()

    def path(self, app_id: str) -> Path:
        if not re.fullmatch(r"[0-9a-f]{16}", app_id):
            raise KeyError(app_id)
        return self.apps / f"{app_id}.json"

    def load(self, app_id: str) -> dict:
        path = self.path(app_id)
        if not path.exists():
            raise KeyError(app_id)
        return json.loads(path.read_text("utf-8"))

    def save(self, app: dict) -> None:
        app["updated_at"] = now_iso()
        path = self.path(app["id"])
        tmp = path.with_suffix(".tmp")
        tmp.write_text(json.dumps(app, indent=2, sort_keys=True) + "\n", "utf-8")
        os.replace(tmp, path)

    def all(self) -> list[dict]:
        return [json.loads(p.read_text("utf-8")) for p in sorted(self.apps.glob("*.json"))]

    def create(self) -> tuple[dict, str]:
        app_id = secrets.token_hex(8)
        token = secrets.token_urlsafe(18)
        app = {
            "id": app_id,
            "service": facts.SERVICE,
            "status": "draft",
            "reference_code": None,
            "confirmation_number": None,
            "created_at": now_iso(),
            "submitted_at": None,
            "answers": {step: {} for step in ("personal", "address", "employment", "review")},
            "uploads": [],
            "sessions": [{"token": token, "started_at": now_iso(), "ended_at": None, "how": "started"}],
            "events": [{"at": now_iso(), "type": "started"}],
        }
        self.save(app)
        return app, token

    def by_token(self, token: str) -> dict | None:
        for app in self.all():
            for s in app["sessions"]:
                if secrets.compare_digest(s["token"], token):
                    return app
        return None

    def by_code(self, code: str) -> dict | None:
        for app in self.all():
            if app.get("reference_code") == code:
                return app
        return None


def event(app: dict, kind: str, **detail) -> None:
    app["events"].append({"at": now_iso(), "type": kind, **detail})


def session_live(app: dict, token: str) -> bool:
    return any(s["token"] == token and s["ended_at"] is None for s in app["sessions"])


def first_incomplete(app: dict) -> str:
    for step in ("personal", "address", "employment"):
        if validate(step, app["answers"].get(step, {})):
            return step
    if upload_problems(app["uploads"]):
        return "documents"
    return "review"


# ----------------------------------------------------------------------------
# HTML
# ----------------------------------------------------------------------------

STYLE = """
:root { color-scheme: light; --ink: #1d232b; --muted: #57606b; --line: #b8c0c8; --accent: #2c4e70;
  --error: #a3262a; --bg: #ffffff; --panel: #f2f4f6; }
* { box-sizing: border-box; }
body { margin: 0; font: 17px/1.5 system-ui, sans-serif; color: var(--ink); background: var(--bg); }
header { background: var(--accent); color: #fff; padding: 12px 16px; }
header p { margin: 0; } header .svc { font-size: 15px; opacity: .9; }
main { max-width: 720px; margin: 0 auto; padding: 24px 16px 48px; }
footer { max-width: 720px; margin: 0 auto; padding: 16px; color: var(--muted); font-size: 14px;
  border-top: 1px solid var(--line); }
h1 { font-size: 30px; line-height: 1.2; margin: 8px 0 20px; }
.step { color: var(--muted); margin: 0; }
.field { margin: 0 0 22px; }
.field label, legend { display: block; font-weight: 600; }
.hint { color: var(--muted); margin: 2px 0 6px; }
.error-text { color: var(--error); font-weight: 600; margin: 2px 0 6px; }
.field.invalid { border-left: 4px solid var(--error); padding-left: 12px; }
input[type=text], input[type=email], input[type=tel] { font: inherit; padding: 6px 8px; width: 100%;
  border: 2px solid var(--ink); }
input.short { max-width: 16em; }
.check { display: flex; gap: 10px; align-items: flex-start; }
.check input { width: 22px; height: 22px; margin-top: 2px; }
.check label { font-weight: 400; }
fieldset { border: 0; padding: 0; margin: 0 0 22px; }
.error-summary { border: 4px solid var(--error); padding: 12px 16px; margin-bottom: 24px; }
.error-summary h2 { margin: 0 0 8px; font-size: 20px; }
.error-summary a { color: var(--error); font-weight: 600; }
.notice { background: var(--panel); padding: 12px 16px; margin-bottom: 20px; }
.code { font: 600 28px/1.3 ui-monospace, monospace; letter-spacing: .05em; }
.actions { display: flex; flex-wrap: wrap; gap: 12px; margin-top: 28px; }
button { font: inherit; padding: 8px 16px; border: 0; background: var(--accent); color: #fff; cursor: pointer; }
button.secondary { background: var(--panel); color: var(--ink); border: 2px solid var(--line); }
button.link { background: none; color: var(--accent); text-decoration: underline; padding: 0; }
dl.answers { display: grid; grid-template-columns: minmax(8em, 1fr) 2fr; gap: 6px 16px; }
dl.answers dt { font-weight: 600; } dl.answers dd { margin: 0; overflow-wrap: anywhere; }
ul.files { padding-left: 20px; }
"""


def esc(value: object) -> str:
    return html.escape(str(value), quote=True)


def page(title: str, body: str, errors: bool = False) -> bytes:
    prefix = "Error: " if errors else ""
    doc = f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{prefix}{esc(title)} - {esc(facts.SERVICE)} - {esc(facts.AUTHORITY)}</title>
<style>{STYLE}</style>
</head>
<body>
<header><p><strong>{esc(facts.AUTHORITY)}</strong></p><p class="svc">Apply for a {esc(facts.SERVICE)}</p></header>
<main id="main">
{body}
</main>
<footer>A fictional service for testing software. Northvale is not a real place, this office does not exist,
and nothing you enter here goes anywhere but this computer.</footer>
</body>
</html>
"""
    return doc.encode("utf-8")


def error_summary(errors: dict[str, str]) -> str:
    if not errors:
        return ""
    items = "".join(f'<li><a href="#f-{esc(k)}">{esc(v)}</a></li>' for k, v in errors.items())
    return (f'<div class="error-summary" role="alert" tabindex="-1"><h2>There is a problem</h2>'
            f"<ul>{items}</ul></div>")


def render_field(f: Field, values: dict[str, str], errors: dict[str, str]) -> str:
    fid = f"f-{f.name}"
    err = errors.get(f.name)
    described = []
    hint = ""
    if f.hint:
        hint = f'<p class="hint" id="{fid}-hint">{esc(f.hint)}</p>'
        described.append(f"{fid}-hint")
    error = ""
    if err:
        error = f'<p class="error-text" id="{fid}-error">Error: {esc(err)}</p>'
        described.append(f"{fid}-error")
    aria = f' aria-describedby="{" ".join(described)}"' if described else ""
    invalid = ' aria-invalid="true"' if err else ""
    cls = "field invalid" if err else "field"
    value = values.get(f.name, "")
    if f.kind == "checkbox":
        checked = " checked" if value == "yes" else ""
        return (f'<div class="{cls}">{error}<div class="check"><input type="checkbox" id="{fid}" name="{f.name}" '
                f'value="yes"{checked}{aria}{invalid}><label for="{fid}">{esc(f.label)}</label></div>{hint}</div>')
    if f.kind == "radio":
        options = []
        for i, (val, label) in enumerate(f.options):
            oid = fid if i == 0 else f"{fid}-{val}"
            checked = " checked" if value == val else ""
            required = " required" if f.required else ""
            options.append(f'<div class="check"><input type="radio" id="{oid}" name="{f.name}" value="{val}"'
                           f'{checked}{required}><label for="{oid}">{esc(label)}</label></div>')
        return (f'<fieldset class="{cls}"{aria}><legend>{esc(f.label)}</legend>{hint}{error}'
                f'{"".join(options)}</fieldset>')
    required = " required" if f.required else ""
    auto = f' autocomplete="{f.autocomplete}"' if f.autocomplete else ' autocomplete="off"'
    short = ' class="short"' if f.width == "short" else ""
    return (f'<div class="{cls}"><label for="{fid}">{esc(f.label)}</label>{hint}{error}'
            f'<input type="{f.kind}" id="{fid}" name="{f.name}" value="{esc(value)}"{short}{required}{auto}'
            f"{aria}{invalid} spellcheck=\"false\"></div>")


def step_heading(step: str) -> str:
    number = STEPS.index(step) + 1
    return f'<p class="step">Step {number} of {len(STEPS)}</p><h1>{esc(STEP_TITLES[step])}</h1>'


SAVE_BUTTON = ('<button type="submit" name="action" value="save" class="secondary" formnovalidate>'
               "Save and continue later</button>")


def step_page(token: str, step: str, values: dict, errors: dict, back: str | None) -> bytes:
    fields = "".join(render_field(f, values, errors) for f in FIELDS[step])
    back_link = f'<p><a href="/s/{esc(token)}/{back}">Back</a></p>' if back else ""
    body = (f"{back_link}{error_summary(errors)}{step_heading(step)}"
            f'<form method="post" action="/s/{esc(token)}/{step}">'
            f'{fields}<div class="actions"><button type="submit" name="action" value="continue">Continue</button>'
            f"{SAVE_BUTTON}</div></form>")
    return page(STEP_TITLES[step], body, bool(errors))


def documents_page(token: str, app: dict, errors: dict[str, str], notice: str = "") -> bytes:
    groups = []
    for key, cat in CATEGORIES.items():
        mine = [u for u in app["uploads"] if u["category"] == key]
        files = ""
        if mine:
            rows = "".join(
                f'<li>{esc(u["filename"])} ({u["size"]:,} bytes) '
                f'<button type="submit" class="link" name="remove" value="{esc(u["id"])}" formnovalidate '
                f'aria-label="Remove {esc(u["filename"])} from {esc(cat["label"])}">Remove</button></li>'
                for u in mine)
            files = f'<p>Uploaded:</p><ul class="files">{rows}</ul>'
        err = errors.get(key)
        error = f'<p class="error-text" id="f-{key}-error">Error: {esc(err)}</p>' if err else ""
        multiple = " multiple" if cat["minimum"] > 1 else ""
        described = f"f-{key}-hint" + (f" f-{key}-error" if err else "")
        groups.append(
            f'<div class="field{" invalid" if err else ""}"><label for="f-{key}">{esc(cat["label"])}</label>'
            f'<p class="hint" id="f-{key}-hint">{esc(cat["hint"])}</p>{error}{files}'
            f'<input type="file" id="f-{key}" name="{key}" accept="{cat["accept"]}"{multiple} '
            f'aria-describedby="{described}"></div>')
    notice_html = f'<div class="notice" role="status">{esc(notice)}</div>' if notice else ""
    body = (f'<p><a href="/s/{esc(token)}/employment">Back</a></p>{error_summary(errors)}{notice_html}'
            f"{step_heading('documents')}"
            "<p>Upload each document under the heading it belongs to. You can upload files one at a time.</p>"
            f'<form method="post" action="/s/{esc(token)}/documents" enctype="multipart/form-data">'
            f'{"".join(groups)}<div class="actions">'
            '<button type="submit" name="action" value="upload" class="secondary">Upload files</button>'
            '<button type="submit" name="action" value="continue">Continue</button>'
            f"{SAVE_BUTTON}</div></form>")
    return page(STEP_TITLES["documents"], body, bool(errors))


def review_page(token: str, app: dict, errors: dict[str, str]) -> bytes:
    sections = []
    for step in ("personal", "address", "employment"):
        answers = app["answers"].get(step, {})
        rows = []
        for f in FIELDS[step]:
            value = answers.get(f.name, "")
            if f.kind == "checkbox":
                value = "Yes" if value == "yes" else "No"
            elif f.kind == "radio":
                value = dict(f.options).get(value, "")
            if value:
                rows.append(f"<dt>{esc(f.label)}</dt><dd>{esc(value)}</dd>")
        sections.append(f'<h2>{esc(STEP_TITLES[step])}</h2><dl class="answers">{"".join(rows)}</dl>'
                        f'<p><a href="/s/{esc(token)}/{step}">Change {esc(STEP_TITLES[step].lower())}</a></p>')
    files = []
    for key, cat in CATEGORIES.items():
        names = ", ".join(u["filename"] for u in app["uploads"] if u["category"] == key) or "None"
        files.append(f"<dt>{esc(cat['label'])}</dt><dd>{esc(names)}</dd>")
    sections.append(f'<h2>Documents</h2><dl class="answers">{"".join(files)}</dl>'
                    f'<p><a href="/s/{esc(token)}/documents">Change documents</a></p>')
    declaration = render_field(FIELDS["review"][0], app["answers"].get("review", {}), errors)
    body = (f'<p><a href="/s/{esc(token)}/documents">Back</a></p>{error_summary(errors)}{step_heading("review")}'
            f'{"".join(sections)}<form method="post" action="/s/{esc(token)}/review">'
            f'<h2>Declaration</h2>{declaration}<div class="actions">'
            '<button type="submit" name="action" value="submit">Submit application</button></div></form>')
    return page(STEP_TITLES["review"], body, bool(errors))


def start_page(errors: dict[str, str] | None = None, code: str = "") -> bytes:
    errors = errors or {}
    resume = render_field(Field("reference_code", "Reference code", hint="For example, NV-ABCD-2345",
                                required=True, width="short"), {"reference_code": code}, errors)
    body = (f"{error_summary(errors)}<h1>Apply for a {esc(facts.SERVICE)}</h1>"
            "<p>Use this service to apply for a permit to rent a home in Northvale. You will need:</p>"
            "<ul><li>your current identity card</li><li>a proof of address from the last three months</li>"
            "<li>your two most recent payslips</li>"
            f"<li>form {esc(facts.FORM_CODE)}, filled in and signed</li></ul>"
            "<p>You can save your application and come back to it with a reference code.</p>"
            '<form method="post" action="/start"><div class="actions">'
            '<button type="submit">Start a new application</button></div></form>'
            '<h2 id="resume">Continue a saved application</h2>'
            f'<form method="post" action="/resume">{resume}<div class="actions">'
            '<button type="submit">Continue application</button></div></form>')
    return page("Start", body, bool(errors))


def message_page(title: str, html_body: str) -> bytes:
    return page(title, f"<h1>{esc(title)}</h1>{html_body}")


# ----------------------------------------------------------------------------
# HTTP
# ----------------------------------------------------------------------------


class Handler(BaseHTTPRequestHandler):
    server_version = "NorthvalePortal/1"
    store: Store
    quiet = False

    def log_message(self, fmt: str, *args) -> None:
        if not self.quiet:
            sys.stderr.write(f"{self.address_string()} {fmt % args}\n")

    # -- responses --
    def send_html(self, body: bytes, status: int = 200) -> None:
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def redirect(self, location: str) -> None:
        self.send_response(HTTPStatus.SEE_OTHER)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def not_found(self) -> None:
        self.send_html(message_page("Page not found", '<p><a href="/">Go to the start</a></p>'), 404)

    def ended(self) -> None:
        self.send_html(message_page(
            "This session has ended",
            "<p>Your answers are saved. To carry on, continue your application with your reference code.</p>"
            '<p><a href="/#resume">Continue a saved application</a></p>'), 410)

    # -- request bodies --
    def body(self, limit: int) -> bytes | None:
        length = int(self.headers.get("Content-Length") or 0)
        if length > limit:
            self.send_html(message_page(
                "The upload is too large",
                "<p>Each file can be up to 5 MB. Go back and choose smaller files.</p>"), 413)
            self.close_connection = True
            return None
        return self.rfile.read(length)

    def form(self) -> tuple[dict[str, str], list[tuple[str, str, bytes]]] | None:
        """Fields (last value wins) and files (field, filename, bytes)."""
        ctype = self.headers.get("Content-Type", "")
        if ctype.startswith("multipart/form-data"):
            raw = self.body(MAX_REQUEST)
            if raw is None:
                return None
            message = email.parser.BytesParser(policy=email.policy.HTTP).parsebytes(
                b"Content-Type: " + ctype.encode("latin-1") + b"\r\n\r\n" + raw)
            fields: dict[str, str] = {}
            files: list[tuple[str, str, bytes]] = []
            for part in message.iter_parts():
                name = part.get_param("name", header="content-disposition")
                filename = part.get_param("filename", header="content-disposition")
                payload = part.get_payload(decode=True) or b""
                if name is None:
                    continue
                if filename is not None:
                    if filename or payload:
                        files.append((name, os.path.basename(filename) or "unnamed", payload))
                else:
                    fields[name] = payload.decode("utf-8", errors="replace").strip()
            return fields, files
        raw = self.body(MAX_FORM)
        if raw is None:
            return None
        parsed = urllib.parse.parse_qs(raw.decode("utf-8", errors="replace"), keep_blank_values=True)
        return {k: v[-1].strip() for k, v in parsed.items()}, []

    # -- routing --
    def route(self) -> tuple[str, str | None, str | None]:
        path = urllib.parse.urlsplit(self.path).path
        m = re.fullmatch(r"/s/([A-Za-z0-9_-]+)/([a-z_]+)", path)
        if m:
            return "step", m.group(1), m.group(2)
        return path, None, None

    def do_GET(self) -> None:
        kind, token, step = self.route()
        if kind == "/healthz":
            body = b"ok\n"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if kind in ("/", "/resume"):
            self.send_html(start_page())
            return
        if kind != "step":
            self.not_found()
            return
        with self.store.lock:
            app = self.store.by_token(token)
            if app is None:
                self.not_found()
                return
            if app["status"] == "submitted":
                self.confirmation(app)
                return
            if not session_live(app, token):
                self.ended()
                return
            if step == "documents":
                self.send_html(documents_page(token, app, {}))
            elif step == "review":
                self.send_html(review_page(token, app, {}))
            elif step in FIELDS:
                self.send_html(step_page(token, step, app["answers"].get(step, {}), {}, self.back(step)))
            else:
                self.not_found()

    def do_POST(self) -> None:
        kind, token, step = self.route()
        if kind == "/start":
            self.form()
            with self.store.lock:
                _, token = self.store.create()
            self.redirect(f"/s/{token}/personal")
            return
        if kind == "/resume":
            parsed = self.form()
            if parsed is None:
                return
            self.resume(parsed[0].get("reference_code", ""))
            return
        if kind != "step":
            self.not_found()
            return
        parsed = self.form()
        if parsed is None:
            return
        values, files = parsed
        with self.store.lock:
            app = self.store.by_token(token)
            if app is None:
                self.not_found()
                return
            if app["status"] == "submitted":
                self.confirmation(app)
                return
            if not session_live(app, token):
                self.ended()
                return
            if step == "documents":
                self.post_documents(app, token, values, files)
            elif step == "review":
                self.post_review(app, token, values)
            elif step in ("personal", "address", "employment"):
                self.post_step(app, token, step, values)
            else:
                self.not_found()

    @staticmethod
    def back(step: str) -> str | None:
        i = STEPS.index(step)
        return STEPS[i - 1] if i > 0 else None

    # -- step handlers --
    def post_step(self, app: dict, token: str, step: str, values: dict[str, str]) -> None:
        answers = {f.name: values.get(f.name, "") for f in FIELDS[step]}
        app["answers"][step] = answers
        action = values.get("action", "continue")
        if action == "save":
            self.save_for_later(app, token, step)
            return
        errors = validate(step, answers)
        if errors:
            event(app, "validation_failed", step=step, fields=errors)
            self.store.save(app)
            self.send_html(step_page(token, step, answers, errors, self.back(step)))
            return
        event(app, "step_completed", step=step)
        self.store.save(app)
        self.redirect(f"/s/{token}/{STEPS[STEPS.index(step) + 1]}")

    def post_documents(self, app: dict, token: str, values: dict[str, str],
                       files: list[tuple[str, str, bytes]]) -> None:
        errors: dict[str, str] = {}
        accepted = []
        if values.get("remove"):
            kept = [u for u in app["uploads"] if u["id"] != values["remove"]]
            for u in app["uploads"]:
                if u["id"] == values["remove"]:
                    event(app, "upload_removed", category=u["category"], filename=u["filename"])
                    try:
                        Path(u["path"]).unlink()
                    except OSError:
                        pass
            app["uploads"] = kept
            self.store.save(app)
            self.send_html(documents_page(token, app, {}, "The file was removed."))
            return
        for name, filename, data in files:
            if name not in CATEGORIES:
                continue
            problem = check_upload(name, filename, data)
            if problem:
                errors[name] = problem
                event(app, "upload_rejected", category=name, filename=filename, reason=problem, size=len(data))
                continue
            upload_id = secrets.token_hex(6)
            folder = self.store.uploads / app["id"] / name
            folder.mkdir(parents=True, exist_ok=True)
            stored = folder / f"{upload_id}-{re.sub(r'[^A-Za-z0-9._-]', '_', filename)}"
            stored.write_bytes(data)
            record = {"id": upload_id, "category": name, "filename": filename, "size": len(data),
                      "sha256": hashlib.sha256(data).hexdigest(), "path": str(stored), "uploaded_at": now_iso()}
            app["uploads"].append(record)
            accepted.append(filename)
            event(app, "upload_accepted", category=name, filename=filename, sha256=record["sha256"])
        action = values.get("action", "upload")
        if action == "save" and not errors:
            self.save_for_later(app, token, "documents")
            return
        if action == "continue" and not errors:
            errors = upload_problems(app["uploads"])
            if not errors:
                event(app, "step_completed", step="documents")
                self.store.save(app)
                self.redirect(f"/s/{token}/review")
                return
        if errors:
            event(app, "validation_failed", step="documents", fields=errors)
        self.store.save(app)
        notice = f"Uploaded {', '.join(accepted)}." if accepted else ""
        self.send_html(documents_page(token, app, errors, notice))

    def post_review(self, app: dict, token: str, values: dict[str, str]) -> None:
        app["answers"]["review"] = {"declaration": values.get("declaration", "")}
        errors: dict[str, str] = {}
        for step in ("personal", "address", "employment"):
            problems = validate(step, app["answers"].get(step, {}))
            if problems:
                errors[f"step-{step}"] = f"{STEP_TITLES[step]}: {next(iter(problems.values()))}"
        for key, problem in upload_problems(app["uploads"]).items():
            errors[f"upload-{key}"] = problem
        errors.update(validate("review", app["answers"]["review"]))
        if errors:
            event(app, "validation_failed", step="review", fields=errors)
            self.store.save(app)
            self.send_html(review_page(token, app, errors))
            return
        app["status"] = "submitted"
        app["submitted_at"] = now_iso()
        app["confirmation_number"] = f"NVP-{today().year}-{secrets.randbelow(900000) + 100000}"
        for s in app["sessions"]:
            s["ended_at"] = s["ended_at"] or now_iso()
        event(app, "submitted", confirmation_number=app["confirmation_number"])
        self.store.save(app)
        self.redirect(f"/s/{token}/confirmation")

    def save_for_later(self, app: dict, token: str, step: str) -> None:
        if not app.get("reference_code"):
            taken = {a.get("reference_code") for a in self.store.all()}
            while True:
                code = "NV-" + "".join(secrets.choice(CODE_ALPHABET) for _ in range(4)) + "-" + \
                       "".join(secrets.choice(CODE_ALPHABET) for _ in range(4))
                if code not in taken:
                    break
            app["reference_code"] = code
        for s in app["sessions"]:
            if s["token"] == token:
                s["ended_at"] = now_iso()
        event(app, "saved", step=step, reference_code=app["reference_code"])
        self.store.save(app)
        self.saved(app)

    def saved(self, app: dict) -> None:
        code = app["reference_code"] or ""
        self.send_html(message_page(
            "Your application is saved",
            f'<div class="notice"><p>Your reference code is</p><p class="code" id="reference-code">{esc(code)}</p>'
            "</div><p>Keep this code. To carry on, go to the start page, choose Continue a saved "
            "application and enter it. This session has ended.</p>"
            '<p><a href="/#resume">Continue a saved application</a></p>'))

    def resume(self, raw: str) -> None:
        code = re.sub(r"\s+", "", raw).upper()
        with self.store.lock:
            if not code:
                self.send_html(start_page({"reference_code": "Enter your reference code"}, raw))
                return
            app = self.store.by_code(code)
            if app is None:
                self.send_html(start_page({"reference_code": "We could not find an application with that reference "
                                                             "code. Check it and try again"}, raw))
                return
            if app["status"] == "submitted":
                self.confirmation(app)
                return
            token = secrets.token_urlsafe(18)
            app["sessions"].append({"token": token, "started_at": now_iso(), "ended_at": None, "how": "resumed"})
            event(app, "resumed", reference_code=code)
            self.store.save(app)
        self.redirect(f"/s/{token}/{first_incomplete(app)}")

    def confirmation(self, app: dict) -> None:
        self.send_html(message_page(
            "Application submitted",
            f'<div class="notice"><p>Your confirmation number is</p>'
            f'<p class="code" id="confirmation-number">{esc(app["confirmation_number"])}</p></div>'
            "<p>We will review your application and write to you within 20 working days.</p>"))


def make_server(state_dir: Path, port: int, host: str = "127.0.0.1", quiet: bool = False) -> ThreadingHTTPServer:
    if host not in ("127.0.0.1", "::1", "localhost"):
        raise SystemExit("the trial portal only listens on the loopback interface")
    store = Store(state_dir)
    handler = type("BoundHandler", (Handler,), {"store": store, "quiet": quiet})
    server = ThreadingHTTPServer((host, port), handler)
    server.daemon_threads = True
    return server


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--state-dir", type=Path, required=True)
    parser.add_argument("--port", type=int, default=facts.PORTAL_PORT)
    parser.add_argument("--host", default="127.0.0.1", help="a loopback address; nothing else is accepted")
    args = parser.parse_args(argv)
    server = make_server(args.state_dir, args.port, args.host)
    print(f"portal listening on http://{args.host}:{server.server_address[1]}/, state in {args.state_dir}",
          flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
