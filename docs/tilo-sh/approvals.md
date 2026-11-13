# Approvals

Some things should not happen because a key said so. Tilo separates actions that only change the
workspace's own records from actions that reach outside it, and the second kind can ask a person first.
Submitting a form to a government site, sending a mail: the call waits, the person decides, and what they
allow runs once.

## What asks, and what does not

| Action | Asks? |
| :--- | :--- |
| A read: listing, reading a record, exporting a file | no |
| A change to a record: an edit, a paragraph, a note | no |
| Making a workspace, minting a key | no: the key already reaches what it is making |
| Asking for a key to be *created* | yes: a new grant of authority is the person's to give |
| A page action in Browser: navigate, fill in, select, click, read back | no, with the key's page scope |
| Something leaving: a submission, a form the page posts, a download it starts, a mail | yes, always |

## How a call waits

A call that needs a person answers **pending**, not failed: it has not run, and it answers the run it
waits under and what the person is being asked. Both SDKs raise that as `AskPending`, carrying the question
and the run.

Nothing has happened yet. The run answers its question as it stands, and the same action asked again under
the same idempotency key puts the question to the person again rather than running it twice. When they
allow it, the run answers `ok`. When they refuse, it answers `failed`, and nothing ran.

A person answers on their own device: their Tilo asks them, with what the action would do.

## Asking for a key

Asking for a key is the clearest case, because the answer is the key itself. Nothing is made yet: the call
raises `AskPending` with a link to show the person, and waiting on it is the key once they approve it.

```python example=ask-for-key
from tilo import AskPending, Tilo

tilo = Tilo()

try:
    tilo.keys.create("Nightly job", expires_in=30 * 24 * 60 * 60)
    print("a key was made without asking anyone")
except AskPending as ask:
    print("waiting for the person at", ask.url)
    print("ask:", ask.invocation_id)
    # ask.wait() follows the approval until they decide, and answers the key.
    key = ask.wait()
    print("the key is", key.grant)
```

```typescript example=ask-for-key
import { AskPending, Tilo } from "tilo-sdk";

const tilo = new Tilo();

try {
  await tilo.keys.create("Nightly job", null, 30 * 24 * 60 * 60);
  console.log("a key was made without asking anyone");
} catch (error) {
  if (!(error instanceof AskPending)) {
    throw error;
  }
  console.log("waiting for the person at", error.url);
  console.log("invocation:", error.invocationId);
  // await error.wait() follows it until they decide, and answers the key.
}
```

## The hosted Browser

The case the design is for: an agent fills a web form in a workspace's hosted Browser, and a person
approves the submission.

Filling asks nobody, when the key carries the page scope: navigate, fill in, select, click, read back,
capture a picture. Then the page is about to send something — a submit button, Enter, the page's own
`submit()`, a `fetch` with a method other than `GET`. The Browser holds the request where it is, and the
call answers that it needs the person, naming what would leave: the page it leaves from, the method, where
it goes, and a picture of the filled page. Their allow releases exactly that request, as filled; a refusal
drops it and the page stays as it was.

The whole flow, with the person answering, is in [the hosted Browser](hosted-browser.md).

> [!NOTE]
> A key that may not operate web pages is asked about every page action, not only the submissions. That is
> the page scope on the key ([keys](identities.md#the-page-scope)), off.

## Where next

- [Runs and history](runs-and-history.md): what the approved action recorded.
- [Errors](errors.md): `AskPending` among the rest.
