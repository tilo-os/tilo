# Agent trial: documents and forms

A self-contained trial for an outside person's AI agent on a hosted Tilo
desktop. The agent drives Tilo's apps through their semantic actions, never
through pixels, on two families of work:

- **Document preparation:** correct a letter, fill a PDF form, put pages in
  order, combine documents, then file the results.
- **An application portal:** fill a long multi-step web form from those
  documents, save it and resume it, fix what the server refuses, upload files
  under the right headings and submit.

Everything in it is fictional. The Municipal Permit Office of Northvale, its
Residential Tenancy Permit, the applicant Mara Quillon, her employer and her
landlord are invented and imitate no real authority, organisation or person.
Every document and portal page says so.

## What is in the pack

| File | What it does |
|---|---|
| `TASKS.md` | Seven task cards: the instruction given to the agent, the files it needs, and what counts as success. |
| `facts.py` | The one set of facts the documents, the portal and the checker share. |
| `generate.py` | Writes the source documents, identical to the byte on every run. Nothing binary is committed. |
| `pdfmini.py` | Just enough PDF, standard library only, to write the documents and read what the agent made. |
| `portal.py` | The mock portal: a Python standard library HTTP server on 127.0.0.1 only. |
| `check.py` | Checks each task from the files and the portal's state. |
| `install.sh`, `reset.sh` | Install the trial for one account, and wipe it for the next attempt. |
| `tests/test_pack.py` | Runs generator, portal and checker end to end against a right and a wrong solution. |

The documents (`~/Documents/Trial/Sources`):

- two identity card scans as image-only PDFs, the current card and an expired one;
- a utility bill (proof of address);
- three payslips: the two latest months and one from a year earlier;
- an employer letter with five misspellings and a wrong start date, as `.docx`
  for the Word Processor and as a PDF;
- a fillable application form (AcroForm text fields and checkboxes);
- a supporting evidence PDF with its pages out of order, a blank page and a
  page upside down.

The facts agree across every document, so each answer can be checked; the
traps are the expired card, the year-old payslip and the evidence's disorder.

## Install it in the VM

The pack needs `python3` 3.9 or newer, which the Tilo image has, and nothing
else. As the trial account (`tester` here), copy this directory to the VM and
run its installer from a login whose systemd user manager runs, such as an ssh
login while `tester` is signed in at the desktop:

```bash
tar -C examples/agent-trials -c documents-and-forms | ssh tester@HOST 'mkdir -p trial-pack && tar -C trial-pack -x'
ssh tester@HOST trial-pack/documents-and-forms/install.sh
```

The installer:

- copies the pack to `~/.local/share/tilo-trial/documents-and-forms`;
- writes the documents to `~/Documents/Trial/Sources`, with empty `Out` and
  `Submission` folders beside them;
- installs the portal as the systemd user service `tilo-trial-portal`, serving
  `http://127.0.0.1:8471/` on the loopback interface only, with its state in
  `~/.local/state/tilo-trial/documents-and-forms`.

Run it again to update the pack; it keeps the participant's outputs and the
portal's state. `install.sh --no-service` skips the service and prints the
command that starts the portal by hand. Without a running user manager, an
administrator can keep one for the account with `loginctl enable-linger tester`.

## Reset it between attempts

```bash
ssh tester@HOST .local/share/tilo-trial/documents-and-forms/reset.sh
```

This stops the portal, deletes its applications and uploads, deletes all of
`~/Documents/Trial`, writes the documents again and restarts the portal. It
does not touch the desktop: close the last attempt's windows (the Browser tab,
open documents) and remove the last participant's agent from the workspace
before the next one starts.

## Before the participant starts: the desktop

Someone at the desktop does three things once.

1. **Give the apps the trial folder.** Apps run sandboxed and see only what the
   workspace grants. The Browser opens an upload as its own sandbox sees the
   path, so a file under `~/Documents/Trial` can be uploaded only once the
   workspace grants the Browser that folder. In the workspace's settings,
   grant Browser, PDF, Word Processor and Files access to files under
   `/home/tester/Documents/Trial` (or approve the access prompt the first time
   an app reaches the folder, scoped to the workspace). A grant takes effect
   the next time the app starts, so restart any of them already open.
   Without a grant, the only files the Browser can upload are the ones in its
   own folder, `~/.local/share/tilo-rooms/<workspace or personal>/so.tilo.browser/`,
   which it sees as `/run/tilo/app/`; copying the upload files there and
   telling the agent those paths is the fallback.
2. **Admit the agent.** When the participant's agent first connects, the
   desktop asks whether to admit it to the workspace. Admit it once.
3. **Set its access.** In the workspace's settings, under agent access, choose
   the agent and allow Outside effects for Browser, PDF and Files (reading a
   signed-in page, filling it and uploading count as outside effects), and
   Changes for Word Processor and PDF. Sending, publishing and deleting for
   good still ask the person at the desktop first.

## How the participant's agent connects

The agent reaches the desktop through `tilo-runtime-mcp --connect` over ssh;
the participant needs ssh access as `tester`. For Claude Code:

```bash
claude mcp add tilo -- ssh -T tester@HOST tilo-runtime-mcp --connect
```

Any MCP client that can run a command works the same way. The agent gets the
session's agent tools: `tilo_find` to find apps, documents and actions,
`tilo_ls` and `tilo_read` to browse resources, and `tilo_do` to invoke an
action by its path, for example `notes/Todo/body#append-paragraph`. It never
sees the screen unless it asks an app for a picture, such as the PDF viewer's
`look-page`.

## Actions agents typically use

Every id below is on the current main branch.

- **Browser** (`browser.page` and the browser itself): `navigate`,
  `read-page` (an outline whose fields carry refs), `find-page-element`,
  `fill-page`, `select-page`, `click-page`, `submit-page`, `press-page`,
  `fill-page-files` (absolute paths, as the Browser's sandbox sees them),
  `wait-page-settled`, `read-page-changes`, `capture-page`.
- **PDF:** `open-pdf`, `read-text`, `look-page` (the identity scans have no
  text layer, so the agent must look at them), `field-edit` on
  `form_values/<name>` to fill a form field, `move-page`, `delete-page`,
  `rotate-page`, `duplicate-page`, `import-pages`, `add-typed-signature`,
  `save-as`.
- **Files:** `move`, `copy-into`, `open-with`.
- **Word Processor:** `open-document`, `read-text`, `find`, `replace-text`,
  `type-text`, `save`, `save-as` (the extension picks the format: `.docx`,
  `.odt`, `.pdf`, `.txt`), `read-file`, `convert-file`.

There is no `find-text` action on the Word Processor; `find` and
`replace-text` do that job.

Not yet on main: a PDF action that lists a form's fields and one that inserts
pages from a file path. Until they land, an agent reads the form with
`read-text` and `look-page`, and combines files through `import-pages`.

## Checking the results

```bash
python3 ~/.local/share/tilo-trial/documents-and-forms/check.py          # every task
python3 ~/.local/share/tilo-trial/documents-and-forms/check.py 2 7      # only these
python3 ~/.local/share/tilo-trial/documents-and-forms/check.py --json
```

It prints PASS or FAIL per task with the reason, and exits non-zero when any
task fails. If the agent reported its reference code or confirmation number
instead of writing the file, pass it with `--reference-code` or
`--confirmation`. A task whose subject the checker cannot observe (a missing
file, an unreadable PDF, no portal state) fails and says so; nothing is
skipped.

What it verifies:

- **Form fields:** each field's value in the saved PDF, read from the file's
  AcroForm (incremental saves and object streams included); case and spacing
  ignored, the income compared as a number, the checkboxes ticked or clear.
- **Page order:** each page is recognised by its own text (a payslip's pay
  period, an evidence page's footer), blank pages by having neither text nor
  pictures, and every page must be shown upright.
- **The letter:** the corrected paragraphs' text in the `.docx`/`.odt` and in
  the exported PDF, with spacing and line breaks ignored.
- **Filing:** which files are in `Submission`, by content hash against the
  originals for the copies, and that the moved files left `Out` while the
  originals stayed in `Sources`.
- **The portal:** its JSON state (`applications/*.json`): every step's
  answers, run through the portal's own validation and compared with the
  facts; that the application was saved and then resumed; the uploads in each
  category, by content hash against the source documents, and the uploaded
  form's fields; the submission and its confirmation number.

What it cannot verify:

- **How** the agent did it. It sees outcomes, not whether the agent used
  semantic actions or something else; the desktop's agent history shows that.
- **Signatures.** It does not check that the form carries a typed signature,
  only its fields.
- **Appearance.** It reads text, field values and page rotation, not pixels:
  a field filled but drawn wrongly, or a page cropped oddly, passes.
- **PDFs it cannot parse:** encrypted files, filters other than Flate, and
  composite fonts without a text map fail with that reason, even if a person
  could read them.
- **Documents saved as something other than PDF, `.docx` or `.odt`** for the
  letter.

## Running the tests

```bash
python3 examples/agent-trials/documents-and-forms/tests/test_pack.py
```

They generate the documents into a temporary home, run the portal on a free
loopback port, drive it as a browser would (including the refusals and the
save and resume), write a correct and a wrong solution and check that the
checker passes the one and names each fault in the other.
