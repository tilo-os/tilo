# Task cards

Seven tasks, from easy to hard. Give the agent the **Instruction** text exactly
as written. The trial runs as the account `tester`; for another account,
replace `/home/tester` throughout. Treat 20 October 2026 as today: the
documents and the portal are set on that date.

The applicant, her employer, her landlord, Northvale and its permit office
are fictional. Every answer is in the source documents in
`/home/tester/Documents/Trial/Sources`:

| File | What it is |
|---|---|
| `id-card-scan-a.pdf`, `id-card-scan-b.pdf` | Identity card scans, pictures with no text layer. One card expired in 2022. |
| `utility-bill.pdf` | A utility statement, proof of the current address, with the applicant's email and phone. |
| `payslip-1.pdf`, `payslip-2.pdf`, `payslip-3.pdf` | Three payslips. Two are the latest months, one is from a year earlier. |
| `employer-letter.docx`, `employer-letter.pdf` | A confirmation of employment with five misspellings and the wrong start date. |
| `application-form-NV-RT1.pdf` | A fillable form with 16 text fields and 3 checkboxes. |
| `supporting-evidence.pdf` | Four evidence pages out of order, one blank page, one page upside down. |

`python3 ~/.local/share/tilo-trial/documents-and-forms/check.py` checks every
task. Name tasks to check only those, for example `check.py 2 4`. The README
explains what the checker can and cannot verify.

---

## 1. Correct the employer letter (easy)

**Instruction**

> The employer letter at /home/tester/Documents/Trial/Sources/employer-letter.docx has five spelling
> mistakes and the wrong employment start date. Open it in the Word Processor, correct the spelling,
> and change the start date to the one on Mara Quillon's payslips. Change nothing else. Save the
> corrected letter as /home/tester/Documents/Trial/Out/employer-letter-corrected.docx and also export
> it as /home/tester/Documents/Trial/Out/employer-letter-corrected.pdf.

**Needs:** `employer-letter.docx`, any payslip (for the start date).

**Succeeds when:**
- `Out/employer-letter-corrected.docx` (or `.odt`) no longer contains `employd`, `Supervsior`,
  `permanant`, `anual` or `recieves`, nor the date `13 January 2002`;
- it gives the start date as `13 January 2020`, and every other paragraph is unchanged;
- `Out/employer-letter-corrected.pdf` holds the same corrected text.

## 2. Fill in the application form (easy)

**Instruction**

> Fill in the application form /home/tester/Documents/Trial/Sources/application-form-NV-RT1.pdf for
> Mara Quillon, using the documents in the same folder. Use her current identity card, not the
> expired one. Write every value in the format the form asks for, tick only the boxes that are true
> for her, enter 20/10/2026 as the date signed and add a typed signature. Save it as
> /home/tester/Documents/Trial/Out/application-form-filled.pdf, keeping the form fields.

**Needs:** both identity scans (to tell them apart; the current card has the current address, the
expired one the previous address), `utility-bill.pdf` (email and phone), a current payslip, page 3 of
the supporting evidence (the previous address and its dates), `application-form-NV-RT1.pdf`.

**Succeeds when** `application-form-filled.pdf` (in `Out` or, after task 5, `Submission`) still has
its fields and they hold:

| Field | Value |
|---|---|
| `applicant_full_name` | Mara Elisabeth Quillon |
| `date_of_birth` | 17/04/1988 |
| `id_card_number` | NVC-5521-8830 (not the expired NVC-1097-4412) |
| `id_card_expiry` | 31/01/2033 |
| `current_address`, `town`, `postcode` | 14 Larkspur Row, Northvale, NV4 7QD |
| `moved_in` | 03/2024 |
| `lived_here_three_years` | clear: she moved in under three years ago |
| `previous_address` | 3 Tannery Lane, Eastmere, EM2 5HB |
| `currently_employed` | ticked |
| `employer_name`, `job_title` | Halberd & Finch Joinery Ltd, Workshop Supervisor |
| `employment_start` | 13/01/2020 |
| `monthly_gross_income` | 3250.00 |
| `email`, `phone` | mara.quillon@example.net, 07700 900418 |
| `declaration` | ticked |
| `date_signed` | any date as DD/MM/YYYY |

Case and spacing are ignored; the income is compared as a number.

## 3. Put the supporting evidence in order (medium)

**Instruction**

> /home/tester/Documents/Trial/Sources/supporting-evidence.pdf is a scan whose pages are out of
> order. It also has one blank page and one page that is upside down. The footer of each page says
> which page it is. Put the pages in order, delete the blank page, turn the upside-down page the
> right way up and save the result as
> /home/tester/Documents/Trial/Out/supporting-evidence-ordered.pdf.

**Needs:** `supporting-evidence.pdf`.

**Succeeds when** the output has exactly four pages, evidence pages 1, 2, 3 and 4 in that order, no
blank page, and no page shown rotated.

## 4. Combine the income evidence (medium)

**Instruction**

> Make one PDF of Mara Quillon's income evidence: her two most recent payslips, the older one first,
> followed by the bank statement page from
> /home/tester/Documents/Trial/Sources/supporting-evidence.pdf. The payslips are in the same folder;
> one of the three is from the wrong year. Save it as
> /home/tester/Documents/Trial/Out/income-evidence.pdf.

**Needs:** the three payslips, `supporting-evidence.pdf`.

**Succeeds when** `income-evidence.pdf` (in `Out` or `Submission`) has exactly three upright pages:
the August 2026 payslip (`payslip-3.pdf`), the September 2026 payslip (`payslip-1.pdf`) and the
September 2026 bank statement (evidence page 4). The September 2025 payslip must not be in it.

## 5. File the submission (medium; after tasks 2 and 4)

**Instruction**

> Move application-form-filled.pdf and income-evidence.pdf from /home/tester/Documents/Trial/Out
> into /home/tester/Documents/Trial/Submission. Then copy Mara Quillon's current identity card scan
> and her utility bill from /home/tester/Documents/Trial/Sources into Submission as well. When you
> are done, Submission holds these four files and nothing else, and Sources still has all its files.

**Needs:** the outputs of tasks 2 and 4, `id-card-scan-b.pdf`, `utility-bill.pdf`.

**Succeeds when** `Submission` holds exactly four files: the filled form and the combined PDF (moved:
gone from `Out`, and each passing its own task's check), a byte-identical copy of the current
identity scan (under any name) and of the utility bill. The expired card's scan must not be there.
`Sources` still holds both originals unchanged.

## 6. Start the portal application and save it (medium)

**Instruction**

> In the Browser, open http://127.0.0.1:8471/ and start an application for a Residential Tenancy
> Permit for Mara Quillon, using the documents in /home/tester/Documents/Trial/Sources. Complete
> "Your details" and "Address history". When you reach "Employment and income", choose "Save and
> continue later". Write the reference code you are given into
> /home/tester/Documents/Trial/Out/reference-code.txt (the Word Processor can save a .txt file).

**Needs:** the current identity scan, `utility-bill.pdf`, page 3 of the supporting evidence.

**Succeeds when** the reference code in `reference-code.txt` (or the code the agent reported, given
to the checker as `--reference-code`) names an application the portal saved, whose details and
address the portal accepts and match the documents: the current card (number NVC-5521-8830,
expiry 31/01/2033), postcode NV4 7QD, moved in 03/2024, the box for three years or more left clear,
and the previous address 3 Tannery Lane, Eastmere, EM2 5HB, from 05/2019 to 02/2024.

The portal expects dates as DD/MM/YYYY and months as MM/YYYY. Typing dates as the card prints them
("17 APR 1988") or entering the expired card is refused with an error message to read and fix.

## 7. Resume, upload and submit (hard; after task 6)

**Instruction**

> Go to http://127.0.0.1:8471/ and continue the saved application whose reference code is in
> /home/tester/Documents/Trial/Out/reference-code.txt. Complete "Employment and income". On "Upload
> documents", upload each file under the heading it belongs to: Mara Quillon's current identity card
> scan, her utility bill as proof of address, her two most recent payslips as proof of income, and
> the form you filled in (/home/tester/Documents/Trial/Out/application-form-filled.pdf, or the copy in
> Submission) as the signed application form. Fix anything the portal refuses. Check your answers,
> accept the declaration and submit. Write the confirmation number into
> /home/tester/Documents/Trial/Out/confirmation.txt.

**Needs:** a current payslip, the source files above and the output of task 2.

**Succeeds when** the same application:
- was resumed with its reference code after it was saved (the old session ends on save, so this
  cannot be skipped);
- is submitted, and `confirmation.txt` (or `--confirmation`) holds its confirmation number;
- holds all the answers of task 6 plus employed, Halberd & Finch Joinery Ltd, Workshop Supervisor,
  started 13/01/2020, monthly income 3250.00 and no other income;
- has exactly the current identity scan under identity, exactly the utility bill under proof of
  address, exactly the August and September 2026 payslips under income, and one PDF under signed
  application form whose fields pass task 2's check.

The portal refuses an income written as `3,250.00`, a payslip given as a `.docx`, any file over
5 MB and the expired identity card's date. The checker reports how many refusals the agent met on
the way.
