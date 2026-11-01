"""The one set of facts every document, the portal's answer key and the checker share.

Everything here is fictional: Northvale, its permit office, the employer, the
landlord and the applicant are invented for this trial and imitate no real
place, authority, company or person. Change a fact here and the documents, the
task cards' answers and the checker move together.
"""

from __future__ import annotations

AUTHORITY = "Municipal Permit Office of Northvale"
SERVICE = "Residential Tenancy Permit"
FORM_CODE = "NV-RT1"

# The day the trial is set on: the applicant applies in October 2026. The
# portal validates against this date rather than the clock, so "moved in
# under three years ago" and "card still valid" answer the same in any year.
APPLICATION_MONTH = "October 2026"
TRIAL_TODAY = (2026, 10, 20)

APPLICANT = {
    "first_name": "Mara",
    "middle_name": "Elisabeth",
    "last_name": "Quillon",
    "full_name": "Mara Elisabeth Quillon",
    "date_of_birth": "17/04/1988",
    "date_of_birth_long": "17 APR 1988",
    "email": "mara.quillon@example.net",
    "phone": "07700 900418",
}

CURRENT_ID = {
    "number": "NVC-5521-8830",
    "issued": "01/02/2023",
    "expires": "31/01/2033",
    "issued_long": "01 FEB 2023",
    "expires_long": "31 JAN 2033",
    "address": "14 LARKSPUR ROW, NORTHVALE NV4 7QD",
}

# The trap: an older card, expired, still in the folder. Its address is the
# applicant's previous one.
EXPIRED_ID = {
    "number": "NVC-1097-4412",
    "issued": "01/01/2013",
    "expires": "31/12/2022",
    "issued_long": "01 JAN 2013",
    "expires_long": "31 DEC 2022",
    "address": "3 TANNERY LANE, EASTMERE EM2 5HB",
}

CURRENT_ADDRESS = {
    "line1": "14 Larkspur Row",
    "town": "Northvale",
    "postcode": "NV4 7QD",
    "moved_in": "03/2024",
    "moved_in_long": "1 March 2024",
}

PREVIOUS_ADDRESS = {
    "line1": "3 Tannery Lane",
    "town": "Eastmere",
    "postcode": "EM2 5HB",
    "moved_in": "05/2019",
    "moved_out": "02/2024",
    "single_line": "3 Tannery Lane, Eastmere, EM2 5HB",
}

EMPLOYER = {
    "name": "Halberd & Finch Joinery Ltd",
    "address": "Unit 7, Millrace Yard, Northvale NV2 3LP",
    "job_title": "Workshop Supervisor",
    "start": "13/01/2020",
    "start_long": "13 January 2020",
    "wrong_start_long": "13 January 2002",
    "employee_number": "HF-0419",
    "signatory": "Owen Petrakis",
    "signatory_title": "Operations Manager",
}

GROSS_MONTHLY = "3250.00"
ANNUAL_SALARY = "39,000.00"

# Two current payslips and a trap from the same month a year earlier. The
# file names do not say which is which: the agent reads them.
PAYSLIPS = [
    {"file": "payslip-1.pdf", "period": "September 2026", "paid": "30/09/2026",
     "gross": "3,250.00", "tax": "468.20", "insurance": "210.40", "net": "2,571.40", "current": True},
    {"file": "payslip-2.pdf", "period": "September 2025", "paid": "30/09/2025",
     "gross": "3,050.00", "tax": "431.80", "insurance": "196.60", "net": "2,421.60", "current": False},
    {"file": "payslip-3.pdf", "period": "August 2026", "paid": "29/08/2026",
     "gross": "3,250.00", "tax": "468.20", "insurance": "210.40", "net": "2,571.40", "current": True},
]

UTILITY_BILL = {
    "supplier": "Northvale Water and Power Cooperative",
    "account": "WP-77310452",
    "date": "05/09/2026",
    "period": "1 August 2026 to 31 August 2026",
    "amount": "84.17",
}

# The supporting evidence, in its right order. The source file holds them as
# SOURCE_EVIDENCE_ORDER says, with a blank page and page 2 upside down.
EVIDENCE = [
    {"key": "EV1", "marker": "Supporting evidence, page 1 of 4", "title": "Tenancy agreement (page 1 of 2)"},
    {"key": "EV2", "marker": "Supporting evidence, page 2 of 4", "title": "Tenancy agreement (page 2 of 2)"},
    {"key": "EV3", "marker": "Supporting evidence, page 3 of 4", "title": "Council tax statement 2026/27"},
    {"key": "EV4", "marker": "Supporting evidence, page 4 of 4", "title": "Bank statement, September 2026"},
]
SOURCE_EVIDENCE_ORDER = ["EV3", "EV1", "BLANK", "EV4", "EV2"]
UPSIDE_DOWN = "EV2"

LANDLORD = "Ardent Lettings Cooperative"

# The employer letter: its errors, and what each must become.
LETTER_TYPOS = {
    "employd": "employed",
    "Supervsior": "Supervisor",
    "permanant": "permanent",
    "anual": "annual",
    "recieves": "receives",
}
LETTER_DATE = "12 October 2026"


def letter_paragraphs(corrected: bool) -> list[str]:
    """The employer letter's paragraphs, with its deliberate errors or fixed."""
    start = EMPLOYER["start_long"] if corrected else EMPLOYER["wrong_start_long"]
    words = {fixed: (fixed if corrected else typo) for typo, fixed in LETTER_TYPOS.items()}
    return [
        EMPLOYER["name"],
        EMPLOYER["address"],
        LETTER_DATE,
        f"To the {AUTHORITY}",
        "Confirmation of employment",
        f"This letter confirms that {APPLICANT['full_name']} has been {words['employed']} by "
        f"{EMPLOYER['name']} since {start} as a Workshop {words['Supervisor']}, "
        f"employee number {EMPLOYER['employee_number']}.",
        f"Her contract is {words['permanent']} and full time. Her {words['annual']} salary is "
        f"{ANNUAL_SALARY} before deductions, and she {words['receives']} it in twelve monthly "
        f"payments of 3,250.00 on the last working day of each month.",
        "Please contact me if you need anything further.",
        "Yours faithfully,",
        EMPLOYER["signatory"],
        f"{EMPLOYER['signatory_title']}, {EMPLOYER['name']}",
    ]


# What the filled form NV-RT1 must hold, by field name.
FORM_TEXT_FIELDS = {
    "applicant_full_name": APPLICANT["full_name"],
    "date_of_birth": APPLICANT["date_of_birth"],
    "id_card_number": CURRENT_ID["number"],
    "id_card_expiry": CURRENT_ID["expires"],
    "current_address": CURRENT_ADDRESS["line1"],
    "town": CURRENT_ADDRESS["town"],
    "postcode": CURRENT_ADDRESS["postcode"],
    "moved_in": CURRENT_ADDRESS["moved_in"],
    "previous_address": PREVIOUS_ADDRESS["single_line"],
    "employer_name": EMPLOYER["name"],
    "job_title": EMPLOYER["job_title"],
    "employment_start": EMPLOYER["start"],
    "monthly_gross_income": GROSS_MONTHLY,
    "email": APPLICANT["email"],
    "phone": APPLICANT["phone"],
}
FORM_CHECKBOXES = {
    "lived_here_three_years": False,
    "currently_employed": True,
    "declaration": True,
}
# Fields the form has that the checker does not hold to one answer.
FORM_FREE_FIELDS = {"date_signed": "DD/MM/YYYY"}

FORM_LABELS = {
    "applicant_full_name": "Full name, as on your identity card",
    "date_of_birth": "Date of birth (DD/MM/YYYY)",
    "id_card_number": "Identity card number",
    "id_card_expiry": "Identity card expiry date (DD/MM/YYYY)",
    "current_address": "Current address, first line",
    "town": "Town",
    "postcode": "Postcode",
    "moved_in": "Moved in (MM/YYYY)",
    "lived_here_three_years": "I have lived at this address for three years or more",
    "previous_address": "Previous address, if under three years at this one",
    "currently_employed": "I am currently employed",
    "employer_name": "Employer",
    "job_title": "Job title",
    "employment_start": "Employed since (DD/MM/YYYY)",
    "monthly_gross_income": "Monthly income before deductions (numbers only, like 2500.00)",
    "email": "Email address",
    "phone": "Phone number",
    "declaration": "I declare that the information on this form is true and complete",
    "date_signed": "Date signed (DD/MM/YYYY)",
}

# What a correct portal submission holds, by step and field. The checker
# compares after normalising case and spaces.
PORTAL_ANSWERS = {
    "personal": {
        "first_name": APPLICANT["first_name"],
        "last_name": APPLICANT["last_name"],
        "date_of_birth": APPLICANT["date_of_birth"],
        "id_number": CURRENT_ID["number"],
        "id_expiry": CURRENT_ID["expires"],
        "email": APPLICANT["email"],
        "phone": APPLICANT["phone"],
    },
    "address": {
        "address_line1": CURRENT_ADDRESS["line1"],
        "town": CURRENT_ADDRESS["town"],
        "postcode": CURRENT_ADDRESS["postcode"],
        "moved_in": CURRENT_ADDRESS["moved_in"],
        "lived_three_years": "",
        "previous_address_line1": PREVIOUS_ADDRESS["line1"],
        "previous_town": PREVIOUS_ADDRESS["town"],
        "previous_postcode": PREVIOUS_ADDRESS["postcode"],
        "previous_moved_in": PREVIOUS_ADDRESS["moved_in"],
        "previous_moved_out": PREVIOUS_ADDRESS["moved_out"],
    },
    "employment": {
        "employment_status": "employed",
        "employer_name": EMPLOYER["name"],
        "job_title": EMPLOYER["job_title"],
        "employment_start": EMPLOYER["start"],
        "monthly_gross_income": GROSS_MONTHLY,
        "has_other_income": "",
    },
}

# Where the trial lives on the desktop, relative to the participant's home.
TRIAL_DIR = "Documents/Trial"
SOURCES = "Sources"
OUT = "Out"
SUBMISSION = "Submission"
PORTAL_PORT = 8471
STATE_DIR = ".local/state/tilo-trial/documents-and-forms"
