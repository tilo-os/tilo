# For your product: one workspace per customer

Your backend runs an agent for each of your customers. Give each customer a Tilo workspace of their own:
your agent works on their documents there, with no access to anyone else's. Your people review its work in
Tilo, see what the agent did step by step, and undo any of it.

You need three things, all from one API key:

1. **A key for your backend.** It reaches all your workspaces, makes new ones and mints keys. You approve it
   once in your Tilo, and it lasts until you revoke it.
2. **A workspace per customer**, with their files in it.
3. **A key per customer's agent.** It reaches that one workspace and nothing else, expires within 90 days
   (or sooner), and stops when you revoke it or the key that minted it.

## Get a key

```sh
tilo keys create "Visa backend"     # prints a link: approve it in your Tilo
export TILO_API_KEY=tilo_sk_…       # what it prints once you approved
```

The key is shown once: keep it in your secret store. It works until you revoke it, so your backend does not
break when some date passes. To have it expire anyway, say when: `tilo keys create "Nightly job"
--expires-in 2592000` (seconds).

You can also create a key on [your grants page](https://mcp.tilo.sh/grants). Keys are listed there with the
agents you connected, when each was last used, and which of your devices approved it. If a key leaks,
revoke it there with one click, or with `tilo keys revoke <grant>`: that ends every key it minted too. The
`tilo_sk_` prefix lets secret scanners find a key committed by mistake.

## A workspace per customer

```python example=workspace-per-customer
from tilo import Refused, Tilo

tilo = Tilo()                                             # your backend's key
agents = {}

for applicant in ["Jane Doe", "Amir Haddad"]:             # one per customer, no limit
    workspace = tilo.workspace(tilo.workspaces.create(f"Applicant: {applicant}").id)

    # Their files: the bytes go up once, then an app takes the reference.
    workspace.files.upload(b"a passport scan, in bytes")

    # A key for this applicant's agent: it reaches this workspace alone.
    key = workspace.keys.mint(name=f"{applicant}'s agent")
    agents[applicant] = key
    print(f"{applicant}: MCP at {key.mcp_url}")

# The customer's agent, with only its own key: it writes the cover letter.
jane_key = agents["Jane Doe"]
agent = Tilo(key=jane_key.token)
jane = agent.workspace(jane_key.workspace)
with jane.invocation("Write the cover letter"):
    jane.do("word-processor#append-paragraph", text="Dear Visa Officer,")
    jane.do("word-processor#append-paragraph", text="My passport and application form are enclosed.")
    letter = jane.do("word-processor#export-content", format="docx")

# The same key cannot reach anyone else, or make a workspace of its own.
try:
    agent.workspaces.create("Applicant: someone else")
except Refused as refused:
    print("the agent cannot make workspaces:", refused.code)

# Later, for review: your backend takes the letter out and hands a person the link to it.
review = tilo.workspace("Applicant: Jane Doe")
letter_bytes = review.files.download(letter)
print("the letter is", len(letter_bytes), "bytes; a person opens it at", review.link("word-processor"))
```

```typescript example=workspace-per-customer
import { Refused, Tilo } from "tilo-sdk";
import type { MintedKey } from "tilo-sdk";

const tilo = new Tilo();                                     // your backend's key
const agents: Record<string, MintedKey> = {};

for (const applicant of ["Jane Doe", "Amir Haddad"]) {       // one per customer, no limit
  const workspace = tilo.workspace((await tilo.workspaces.create(`Applicant: ${applicant}`)).id);

  // Their files: the bytes go up once, then an app takes the reference.
  await workspace.files.upload(new TextEncoder().encode("a passport scan, in bytes"));

  // A key for this applicant's agent: it reaches this workspace alone.
  const key = await workspace.keys.mint(`${applicant}'s agent`);
  agents[applicant] = key;
  console.log(`${applicant}: MCP at ${key.mcpUrl}`);
}

// The customer's agent, with only its own key: it writes the cover letter.
const janeKey = agents["Jane Doe"]!;
const agent = new Tilo({ key: janeKey.token });
const jane = agent.workspace(janeKey.workspace);
const letter = await jane.invocation("Write the cover letter", async () => {
  await jane.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
  await jane.do("word-processor#append-paragraph", {
    text: "My passport and application form are enclosed.",
  });
  return jane.do("word-processor#export-content", { format: "docx" });
});

// The same key cannot reach anyone else, or make a workspace of its own.
try {
  await agent.workspaces.create("Applicant: someone else");
} catch (error) {
  if (error instanceof Refused) {
    console.log("the agent cannot make workspaces:", error.code);
  }
}

// Later, for review: your backend takes the letter out and hands a person the link to it.
const review = tilo.workspace("Applicant: Jane Doe");
const letterBytes = await review.files.download(String(letter));
console.log("the letter is", letterBytes.length, "bytes; a person opens it at", await review.link("word-processor"));
```

## Browser use

The agent's key opens the workspace's own Browser with `browser#open`, and then drives the page with the
same generic action call it uses for anything else: the page is a resource, `browser/page`, and every page
action is one call. The window runs in the workspace's own Linux machine, so the call answers once that
machine woke and the window is live.

```python example=hosted-browser host=stack
import os

from tilo import Invocation, Tilo

tilo = Tilo()                                            # your backend's key
made = tilo.workspaces.create("Visa application: Jane Doe")
key = tilo.workspace(made.id).keys.mint("Jane Doe's agent")
workspace = Tilo(key=key.token).workspace(made.id)       # the agent's key, in its one workspace

# The page to drive: any address the workspace's machine reaches.
url = os.environ.get("TILO_BROWSER_URL", "https://visa.example/apply")

# Browser opens in the workspace's own machine; a window that takes longer than one call's wait
# comes back as its run, followed until the window is live.
opened = workspace.do("browser#open")
if isinstance(opened, Invocation):
    opened.result()
workspace.do("browser/page#navigate", url=url)
workspace.do("browser/page#wait-page-settled", quiet_ms=300, timeout_ms=30_000)

# The page is a resource: find the field, fill it, and read the page back.
family = workspace.do("browser/page#find-page-element", role="textbox", name="Family name")
workspace.do("browser/page#fill-page", element=family["element"], text="Doe")
page = workspace.do("browser/page#read-page")
print("the page is", page["title"], "and says:", page["outline"])
```

```typescript example=hosted-browser host=stack
import { Invocation, Tilo } from "tilo-sdk";

const tilo = new Tilo();                                 // your backend's key
const made = await tilo.workspaces.create("Visa application: Jane Doe");
const minted = await tilo.workspace(made.id).keys.mint("Jane Doe's agent");
const workspace = new Tilo({ key: minted.token }).workspace(made.id); // the agent's key, in its one workspace

// The page to drive: any address the workspace's machine reaches.
const url = process.env.TILO_BROWSER_URL ?? "https://visa.example/apply";

// Browser opens in the workspace's own machine; a window that takes longer than one call's wait
// comes back as its run, followed until the window is live.
const opened = await workspace.do("browser#open");
if (opened instanceof Invocation) {
  await opened.result();
}
await workspace.do("browser/page#navigate", { url });
await workspace.do("browser/page#wait-page-settled", { quiet_ms: 300, timeout_ms: 30_000 });

// The page is a resource: find the field, fill it, and read the page back.
const family = (await workspace.do("browser/page#find-page-element", {
  role: "textbox",
  name: "Family name",
})) as { element: string };
await workspace.do("browser/page#fill-page", { element: family.element, text: "Doe" });
const page = (await workspace.do("browser/page#read-page")) as { title: string; outline: string };
console.log("the page is", page.title, "and says:", page.outline);
```

The page's actions are the ones a person's Browser has: `navigate`, `wait-page-settled`, `read-page`,
`find-page-element`, `fill-page`, `select-page`, `click-page`, `capture-page` and `submit-page`
([the hosted Browser](../tilo-sh/hosted-browser.md)). A submit, a click that sends, or a page posting on
its own is put to the person: the SDK raises `AskPending`, and nothing leaves until they allow that one
request. A key minted with `browser_pages` on, as `keys.mint` mints by default, may fill, select and click
without asking; a submission always asks.

## Hundreds of customers: label, find, export

A backend serving many customers keeps its own id for each one on their workspace as a **label**, and finds
the workspace again by it: no table of workspace ids to keep beside your own database. Labels are names
(1 to 63 of `a-z`, `0-9`, `_`, `-` and `.`) to text values, at most 16 per workspace. Only your own keys
read them: nobody you invite into the workspace sees them, and a key minted for it never sets them.

Passing your customer's id as the create's idempotency key makes the create safe to repeat: asked again,
after a timeout or a crash, it answers the workspace the first one made instead of making a second.

To start each customer from a workspace you set up once, branch that one instead of creating an empty
one: `tilo.workspaces.branch(template, name, labels=..., idempotency_key=...)` takes the same labels and
idempotency key, and the customer's workspace starts with everything the template holds and goes on apart
from it ([branch a workspace](../tilo-sh/workspaces.md#branch-a-workspace)).

```python example=customer-labels
from tilo import Tilo

tilo = Tilo()                                              # your backend's key
for customer in ["cus_8121", "cus_8122", "cus_8123"]:     # your own ids for them
    tilo.workspaces.create(
        f"Applicant {customer}",
        labels={"customer_id": customer, "plan": "visa"},
        idempotency_key=f"workspace-for-{customer}",       # a retry makes no second one
    )

jane = tilo.workspaces.find({"customer_id": "cus_8121"})  # None when there is none
print("cus_8121 is", jane.id, jane.labels)
print("visa customers:", len(tilo.workspaces.list(labels={"plan": "visa"})))
print("usage rows:", len(tilo.usage().workspaces))        # what each one keeps, and its VM time

# Before a customer leaves: everything their workspace holds, then delete it.
workspace = tilo.workspace(jane.id)
export = workspace.export()
print("exported", len(export.resources), "resources of", export.workspace.labels["customer_id"])
for gone in tilo.workspaces.list(labels={"plan": "visa"}):
    tilo.workspaces.delete(gone.id)
```

```typescript example=customer-labels
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();                                     // your backend's key
await Promise.all(                                           // all at once: hundreds is fine
  ["cus_8121", "cus_8122", "cus_8123"].map((customer) =>    // your own ids for them
    tilo.workspaces.create(`Applicant ${customer}`, {
      labels: { customer_id: customer, plan: "visa" },
      idempotencyKey: `workspace-for-${customer}`,           // a retry makes no second one
    }),
  ),
);

const jane = (await tilo.workspaces.find({ customer_id: "cus_8121" }))!; // undefined when none
console.log("cus_8121 is", jane.id, jane.labels);
console.log("visa customers:", (await tilo.workspaces.list({ labels: { plan: "visa" } })).length);
console.log("usage rows:", (await tilo.usage()).workspaces.length); // what each keeps, and its VM time

// Before a customer leaves: everything their workspace holds, then delete it.
const exported = await tilo.workspace(jane.id).export();
console.log("exported", exported.resources.length, "resources of", exported.workspace.labels.customer_id);
for (const gone of await tilo.workspaces.list({ labels: { plan: "visa" } })) {
  await tilo.workspaces.delete(gone.id);
}
```

An export is every resource of every app in the workspace, each read whole as `tilo_read` reads it, with the
workspace's listing and labels; a resource that does not read is named in `unread` with why. A page's resources
are read side by side and written in the order a walk finds them, and the document streams out as it is read, so
the server holds no more than the reads in flight however much a workspace holds. Exports of many workspaces run
side by side up to a bound; the rest wait their turn rather than fail. The files the
resources name are `tilo.content:` references: download each with `files.download` (`GET
/v1/workspaces/<workspace>/contents/<hash>`) while the workspace still exists. From a shell,
`tilo workspace create "Applicant cus_8121" --label customer_id=cus_8121` and
`tilo workspace ls --label customer_id=cus_8121` do the same.

[`sdk/typescript/examples/workspace_per_customer.ts`](../../sdk/typescript/examples/workspace_per_customer.ts)
is all of this at a backend's scale: it makes 300 labelled workspaces at once, finds each by its customer
id, mints a key for each, has every customer's agent work over MCP at once with its own token (list, read,
an action, a file put), checks that no token reaches its neighbour's workspace over MCP or REST, revokes a
third of the keys, exports and deletes everything, and prints the timings, every failure with why, and the
host's memory and CPU. Against a local tilo.sh:

```sh
eval "$(os/containers/tilo-sh/stack.sh env)"             # TILO_API_URL, TILO_SITE: the local stack
eval "$(os/containers/tilo-sh/stack.sh owner --here)"    # TILO_API_KEY: the stack's own person, an owner
CUSTOMERS=300 SAMPLE_PIDS=server:$(cat ~/.local/state/tilo-sh-local/run/server.pid) \
  bun sdk/typescript/examples/workspace_per_customer.ts
```

The key is the stack's own person's, and that person is the Lounge's owner from the start, so no
browser is needed (`stack.sh owner --here` approves the key on the spot; `stack.sh env` exports it from
then on). To act as a person of your own instead, open the administrator link `stack.sh up` printed
(`stack.sh owner` prints it again), which makes you an administrator, and approve a key of yours there.
(`TILO_SH_STATE` moves the stack's state from `~/.local/state/tilo-sh-local`.)

## What the agent's key reaches

Hand the agent its MCP URL and token. Any MCP client connects with them: the streamable HTTP server
`https://mcp.tilo.sh/w/<workspace>/mcp`, with the header `Authorization: Bearer <token>`. In a JSON MCP
configuration:

```json
{"mcpServers": {"jane": {"type": "http", "url": "https://mcp.tilo.sh/w/<workspace>/mcp",
  "headers": {"Authorization": "Bearer <token>"}}}}
```

The agent sees that one workspace's apps through the same tools a person's agent gets: `tilo_ls`,
`tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status` and `tilo`. It fills the PDF's form fields
(`pdf-viewer/application.pdf#field-edit`) and writes a cover letter in Writer (`word-processor#append-paragraph`,
then `#export-content` as docx). It cannot list, read or act in any other workspace: another workspace's
MCP URL answers its token `403`, every `/v1/workspaces/<other>/...` route answers `403 beyond_scope`, a
tool call naming another workspace reaches nothing, and the account's paths (`account/...` apart from the
keys its workspace admitted and its list of workspaces: usage, making, deleting, minting) are refused.
`account/keys#list` lists the key and the keys it minted there, from the workspace's own governance. Listing workspaces
with it names its own alone. Revoking the key ends its open MCP sessions at once: their next call is
refused, and the token opens no new one (`401 key_revoked`). Every call that
changed a resource answers that resource's `link`, and `tilo_link {path}` answers the link to anything a path
names, so the agent can end its work by handing a person the link to what it did.

## Review

Open the workspace in Tilo, or invite the person who reviews it:

```sh
tilo workspace people invite --role editor --email ops@example.com -w "Applicant: Jane Doe"
tilo workspace people ls -w "Applicant: Jane Doe"
tilo workspace people name "Jane Doe" -w "Applicant: Jane Doe"   # what its people and your invitations call you
```

Following the link signs them in to Tilo (with a passkey, or by approving an agent) and lands them in the
workspace as that role. No mail is sent: hand the link over yourself. From then on, the resource links your
agent hands over open straight in the resource's app for them.

A resource link is the one a person copies in the app, for example
`https://tilo.sh/app/#/@eu1.tilo.sh/w/<workspace>/so.tilo.pdf-viewer/<record>`. It carries no secret: it
opens only for the workspace's members, and anyone else meets the workspace's admission, so invite the
reviewer first. Your key invites as you would, as far as your role in the workspace goes; an agent's key asks
you first unless you allowed it in the workspace's settings. The reviewer then works with a key of their own: their account lists the
workspace with the role they hold there (`tilo.workspaces.list()`, each row carrying `role`), so they reach
it by name too.

Each agent session is one invocation in History, named after its key. Undo it to take back everything it
changed ([invocations and history](../tilo-sh/invocations-and-history.md)). Watch it live from your backend with
`workspace.watch()` ([events](../tilo-sh/events.md)).

## When a customer leaves

Delete their workspace, and their data goes with it:

```sh
tilo workspace rm "Applicant: Jane Doe"   # {"deleted": id, "revoked_keys": [...]}
```

Deleting is final: there is no undo, and nothing asks again, so the call is the confirmation. The host
removes the workspace's documents, files and history and stops counting them against your storage. Anyone
with it open is told it was deleted, and their Tilo forgets it; a desktop or browser that was away is told
when it is back, also one of someone invited to only that workspace. Every key minted for it is revoked:
its agent's next call raises `KeyRevoked` (`key_revoked` on the wire), which says when. Only a key reaching
all your workspaces deletes, and only workspaces of your own; one you were invited into is `not_found`.

A key the host turns away says why: `KeyRevoked`, `KeyExpired` (a minted key past its expiry,
`key_expired`), or `KeyRefused` with `invalid_token` for one it never issued. All three are a
`ProtocolError` with HTTP status 401 ([errors](../tilo-sh/errors.md)).

## The same from a shell

```sh
tilo workspace create "Applicant: Jane Doe"
tilo workspace branch "Onboarding template" "Applicant: Amir Haddad"   # or start from one set up once
tilo files put passport.png -w "Applicant: Jane Doe"
tilo -w "Applicant: Jane Doe" do pdf-viewer#import-pdf-content content=tilo.content:… name=application.pdf
tilo -w "Applicant: Jane Doe" do pdf-viewer/application.pdf#read-form-fields
tilo -w "Applicant: Jane Doe" do word-processor#export-content format=docx
tilo files get tilo.content:… -w "Applicant: Jane Doe" -o cover-letter.docx
tilo keys mint -w "Applicant: Jane Doe" --name "Jane Doe's agent"
tilo workspace people invite --role editor --email ops@example.com -w "Applicant: Jane Doe"
tilo workspace people ls -w "Applicant: Jane Doe"
tilo keys revoke <grant>
tilo workspace rm "Applicant: Jane Doe"   # when the customer leaves
```

## What is underneath

Making, branching and deleting workspaces and minting keys are the account's
(`account/workspaces#create`, `#branch` and `#delete`, `account/keys#mint`, each run with
`POST /v1/resources/account/<resource>/actions/<action>`), inviting is the
workspace's own (`workspace/invitations#invite`), and everything in a
workspace is a resource under `/v1/workspaces/<workspace>/resources/...`. A minted key is a
certificate your key signed, narrower than it: the
hosts and the runtime check it like any other device of yours, and it ends when your key is revoked
([design](../architecture/workspace-api.md)).

Every one of these is also an MCP call an agent makes (`account/workspaces#create`,
`account/workspaces#branch`, `account/keys#mint`, `workspace/invitations#invite`), answered typed: one API, two ways to say it
([`api.md`](api.md)). A file's bytes go up in one request before an app takes the reference
([files](../tilo-sh/files.md)).
