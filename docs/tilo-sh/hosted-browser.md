# The hosted Browser

A workspace on tilo.sh opens Browser like any other app, and the pages it opens run in the workspace's own
Linux machine — its own VM, its own profile, nobody else's. An agent fills a form there with a key, and a
person approves what leaves.

This is the shape a visa application takes: a backend makes a workspace per applicant, an agent fills the
form on the government's site, and the applicant's reviewer allows the submission.

## Open the window

An agent opens Browser with an ordinary action, `browser#open` — the same one a person's click runs.

The call answers once the window is live: the first open of a sleeping workspace wakes its machine, which
costs seconds, and one that is already up answers in about a second. Opening again attaches to the same
window instead of making a second one. The window is a normal Browser window in the workspace: its members
see it in their window, in the desktop app and on the web page, live.

The window it answers with has a page, and the page's actions are what an agent drives:

```text
browser@<window>/page#navigate
```

## What an agent does with a page

A page offers the actions a person's Browser offers, addressed as paths:

| Action | Takes | Answers |
| :--- | :--- | :--- |
| `navigate` | `url` | the page it went to |
| `wait-page-settled` | `quiet_ms`, `timeout_ms` | that the page has settled |
| `read-page` | — | the page as an outline: its address, its title and its text |
| `find-page-element` | `role`, `name`, `nth` | the `element` ref to act on |
| `fill-page` | `element`, `text` | the field filled |
| `select-page` | `element`, `values` | the choice made |
| `click-page` | `element` | the click |
| `capture-page` | — | a picture of the page |
| `submit-page` | `element` | the submission, once a person allows it |

With a key that carries the page scope — the default — filling a form asks nobody. The agent works like
this, and every step is a call:

```text
navigate https://visa.example/apply
wait-page-settled quiet_ms=300
find-page-element role=textbox name="Family name"      → {"element": "d1e4"}
fill-page element=d1e4 text="Doe"
find-page-element role=combobox name="Nationality"     → {"element": "d1e7"}
select-page element=d1e7 values=["NL"]
find-page-element role=button name="Submit application" → {"element": "d1e9"}
capture-page                                            → a PNG of the filled form
```

`read-page` is how a caller checks its own work: the outline holds the values that are now in the fields,
and the page's own words. After a submission it holds the receipt and the reference number the site made.

## Approve the submission

Then the page is about to leave: `submit-page`, a click on the button that submits, Enter in a field, or
the page posting on its own. The Browser holds the request where it is — paused, not dropped — and the call
answers that it needs the person, naming what would leave:

- the page it leaves from,
- the method (`POST`),
- where it goes,
- a picture of the filled page, so the person sees exactly what is being sent.

The person is asked on their own device, in Tilo. Their **allow** releases that one request, exactly as it
was filled; the call returns and the page goes on to its receipt. Their **refusal**, or nobody answering,
drops it and leaves the page as it was, filled and unsent. Nobody is asked about one departure twice, and
the same call does not run the page action again when the answer comes back.

A departure no action started — a page that posts on its own, a download — waits the same way, and the
window offers the wait as two actions bound to it, `send-held-request` and `keep-held-request`, so a person
(or a call they answer) can settle it.

## What a person sees, and what stays

- **The window is the workspace's.** It browses on the workspace's own profile, so a site the workspace
  signed into stays signed in for that workspace, and none of your own sign-ins are there.
- **It ends by itself**: ten minutes without page activity, or two hours alive. The workspace's machine
  then sleeps like any idle machine, and the next open wakes it.
- **Nothing leaves without the person.** A key can fill and click its way around a page, and cannot send,
  upload a file or type a password without them ([approvals](approvals.md)).
- **Page actions are what is counted.** The account's plan includes a monthly allowance of them; past it a
  page action is refused with `no_payment_method` and the step that adds a card. An action the person did
  not allow is never counted.

## Filling a form without a Browser

Not every form is a web page. When the application is a PDF, the agent fills it in PDF Viewer instead:
`pdf-viewer#import-pdf-content`, `read-form-fields`, a field edit per answer, and
`word-processor#export-content` for the letter that goes with it. A person reviews both, in the same
workspace, with the same undo — [one workspace per
customer](../developers/one-workspace-per-customer.md) is that flow end to end.
