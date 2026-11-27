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
tilo login                            # this computer as one of your devices, approved in Tilo
tilo do account/keys#create name="Visa backend" --out visa.key   # approve the link it prints
export TILO_API_KEY=$(cat visa.key)   # what the SDKs read
```

The key is written once: keep it in your secret store. It works until you revoke it, so your backend does not
break when some date passes. To have it expire anyway, say when: `tilo do account/keys#create
name="Nightly job" expires_in=2592000` (seconds).

You can also create a key on [your grants page](https://mcp.tilo.sh/grants). Keys are listed there with the
agents you connected, when each was last used, and which of your devices approved it. If a key leaks,
revoke it there with one click, or with `tilo do "account/keys/<grant>#revoke"`: that ends every key it minted too. The
`tilo_sk_` prefix lets secret scanners find a key committed by mistake.

## A workspace per customer

```python example=workspace-per-customer
import asyncio

from tilo import Refused, Tilo, run


async def main() -> None:
    async with Tilo() as tilo:                            # your backend's key
        agents = {}

        for applicant in ["Jane Doe", "Amir Haddad"]:     # one per customer, no limit
            made = await run(
                tilo,
                {"path": "account/workspaces", "action": "create"},
                {"name": f"Applicant: {applicant}"},
            )
            workspace = tilo.workspace(made["result"]["workspace"]["id"])

            # Their files: the bytes go up once, then an app takes the reference.
            await workspace.upload(b"a passport scan, in bytes")

            # A key for this applicant's agent: it reaches this workspace alone.
            key = (
                await run(
                    tilo,
                    {"path": "account/keys", "action": "mint"},
                    {"workspace": await workspace.id(), "name": f"{applicant}'s agent"},
                )
            )["result"]
            agents[applicant] = key
            print(f"{applicant}: MCP at {key.get('mcp_url')}")

        # The customer's agent, with only its own key: it writes the cover letter.
        jane_key = agents["Jane Doe"]
        agent = Tilo(key=jane_key["token"])
        jane = agent.workspace(jane_key["workspace"])
        async with jane.invocation("Write the cover letter"):
            await run(
                jane,
                {"path": "word-processor", "action": "append_paragraph"},
                {"text": "Dear Visa Officer,"},
            )
            await run(
                jane,
                {"path": "word-processor", "action": "append_paragraph"},
                {"text": "My passport and application form are enclosed."},
            )
            letter = (
                await run(
                    jane, {"path": "word-processor", "action": "export_document"}, {"format": "docx"}
                )
            ).get("result")

        # The same key cannot reach anyone else, or make a workspace of its own.
        try:
            await run(tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Applicant: someone else"})
        except Refused as refused:
            print("the agent cannot make workspaces:", refused.code)

        # Later, for review: your backend takes the letter out and hands a person the link to it.
        review = tilo.workspace("Applicant: Jane Doe")
        letter_bytes = await review.download(str(letter))
        link = (
            await run(review, {"path": "word-processor", "action": "share-link"})
        )["result"]["url"]
        print("the letter is", len(letter_bytes), "bytes; a person opens it at", link)
        await agent.aclose()


asyncio.run(main())
```

```typescript example=workspace-per-customer
import { Refused, Tilo, run } from "tilo-sdk";

type MintedKey = { grant: string; token: string; workspace?: string; mcp_url?: string };

const tilo = new Tilo();                                     // your backend's key
const agents: Record<string, MintedKey> = {};

for (const applicant of ["Jane Doe", "Amir Haddad"]) {       // one per customer, no limit
  const made = await run(
    tilo,
    { path: "account/workspaces", action: "create" },
    { name: `Applicant: ${applicant}` },
  );
  const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);

  // Their files: the bytes go up once, then an app takes the reference.
  await workspace.upload(new TextEncoder().encode("a passport scan, in bytes"));

  // A key for this applicant's agent: it reaches this workspace alone.
  const minted = await run(
    tilo,
    { path: "account/keys", action: "mint" },
    { workspace: await workspace.id(), name: `${applicant}'s agent` },
  );
  const key = minted.result as MintedKey;
  agents[applicant] = key;
  console.log(`${applicant}: MCP at ${key.mcp_url}`);
}

// The customer's agent, with only its own key: it writes the cover letter.
const janeKey = agents["Jane Doe"]!;
const agent = new Tilo({ key: janeKey.token });
const jane = agent.workspace(janeKey.workspace ?? "");
let letter: unknown;
await jane.invocation("Write the cover letter", async () => {
  await run(
    jane,
    { path: "word-processor", action: "append_paragraph" },
    { text: "Dear Visa Officer," },
  );
  await run(
    jane,
    { path: "word-processor", action: "append_paragraph" },
    { text: "My passport and application form are enclosed." },
  );
  letter = (
    await run(jane, { path: "word-processor", action: "export_document" }, { format: "docx" })
  ).result;
});

// The same key cannot reach anyone else, or make a workspace of its own.
try {
  await run(agent, { path: "account/workspaces", action: "create" }, { name: "Applicant: someone else" });
} catch (error) {
  if (error instanceof Refused) {
    console.log("the agent cannot make workspaces:", error.code);
  }
}

// Later, for review: your backend takes the letter out and hands a person the link to it.
const review = tilo.workspace("Applicant: Jane Doe");
const letterBytes = await review.download(String(letter));
const shared = await run(review, { path: "word-processor", action: "share-link" });
console.log(
  "the letter is",
  letterBytes.length,
  "bytes; a person opens it at",
  (shared.result as { url: string }).url,
);
agent.close();
```

## Browser use

The agent's key opens the workspace's own Browser with `browser#open`, and then drives the page with the
same generic action call it uses for anything else: the page is a resource, `browser/page`, and every page
action is one call. The window runs in the workspace's own Linux machine, so the call answers once that
machine woke and the window is live.

```python example=hosted-browser host=stack
import asyncio
import os

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:                            # your backend's key
        made = (
            await run(
                tilo,
                {"path": "account/workspaces", "action": "create"},
                {"name": "Visa application: Jane Doe"},
            )
        )["result"]["workspace"]
        key = (
            await run(
                tilo,
                {"path": "account/keys", "action": "mint"},
                {"workspace": made["id"], "name": "Jane Doe's agent"},
            )
        )["result"]
        agent = Tilo(key=key["token"])
        workspace = agent.workspace(made["id"])           # the agent's key, in its one workspace

        # The page to drive: any address the workspace's machine reaches.
        url = os.environ.get("TILO_BROWSER_URL", "https://visa.example/apply")

        # Browser opens in the workspace's own machine; `run` follows the window until it is live.
        await run(workspace, {"path": "browser", "action": "open"})
        await run(workspace, {"path": "browser/page", "action": "navigate"}, {"url": url})
        await run(
            workspace,
            {"path": "browser/page", "action": "wait-page-settled"},
            {"quiet_ms": 300, "timeout_ms": 30_000},
        )

        # The page is a resource: find the field, fill it, and read the page back.
        family = (
            await run(
                workspace,
                {"path": "browser/page", "action": "find-page-element"},
                {"role": "textbox", "name": "Family name"},
            )
        )["result"]
        await run(
            workspace,
            {"path": "browser/page", "action": "fill-page"},
            {"element": family["element"], "text": "Doe"},
        )
        page = (await run(workspace, {"path": "browser/page", "action": "read-page"}))["result"]
        print("the page is", page["title"], "and says:", page.get("outline"))
        await agent.aclose()


asyncio.run(main())
```

```typescript example=hosted-browser host=stack
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();                                 // your backend's key
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Visa application: Jane Doe" },
);
const workspaceId = (made.result as { workspace: { id: string } }).workspace.id;
const key = await run(
  tilo,
  { path: "account/keys", action: "mint" },
  { workspace: workspaceId, name: "Jane Doe's agent" },
);
const agent = new Tilo({ key: (key.result as { token: string }).token });
const workspace = agent.workspace(workspaceId);          // the agent's key, in its one workspace

// The page to drive: any address the workspace's machine reaches.
const url = process.env.TILO_BROWSER_URL ?? "https://visa.example/apply";

// Browser opens in the workspace's own machine; `run` follows the window until it is live.
await run(workspace, { path: "browser", action: "open" });
await run(workspace, { path: "browser/page", action: "navigate" }, { url });
await run(
  workspace,
  { path: "browser/page", action: "wait-page-settled" },
  { quiet_ms: 300, timeout_ms: 30_000 },
);

// The page is a resource: find the field, fill it, and read the page back.
const found = await run(
  workspace,
  { path: "browser/page", action: "find-page-element" },
  { role: "textbox", name: "Family name" },
);
const family = found.result as { element: string };
await run(
  workspace,
  { path: "browser/page", action: "fill-page" },
  { element: family.element, text: "Doe" },
);
const read = await run(workspace, { path: "browser/page", action: "read-page" });
const page = read.result as { title: string; outline?: string };
console.log("the page is", page.title, "and says:", page.outline);
agent.close();
```

The page's actions are the ones a person's Browser has: `navigate`, `wait-page-settled`, `read-page`,
`find-page-element`, `fill-page`, `select-page`, `click-page`, `capture-page` and `submit-page`
([the hosted Browser](../tilo-sh/hosted-browser.md)). A submit, a click that sends, or a page posting on
its own is put to the person: the call answers `pending` with the ask, and nothing leaves until they allow
that one request. A key minted with `browser_pages` on, as `account/keys#mint` mints by default, may fill,
select and click without asking; a submission always asks.

## Hundreds of customers: label, find, export

A backend serving many customers keeps its own id for each one on their workspace as a **label**, and finds
the workspace again by it: no table of workspace ids to keep beside your own database. Labels are names
(1 to 63 of `a-z`, `0-9`, `_`, `-` and `.`) to text values, at most 16 per workspace. Only your own keys
read them: nobody you invite into the workspace sees them, and a key minted for it never sets them.

Passing your customer's id as the create's idempotency key makes the create safe to repeat: asked again,
after a timeout or a crash, it answers the workspace the first one made instead of making a second.

To start each customer from a workspace you set up once, branch that one instead of creating an empty
one: `run(tilo, {"path": "account/workspaces", "action": "branch"}, {"workspace": …, "name": …, "labels":
…})` takes the same labels and idempotency key, and the customer's workspace starts with everything the
template keeps and goes on apart from it ([branch a workspace](../tilo-sh/workspaces.md#branch-a-workspace)).

```python example=customer-labels
import asyncio

from tilo import Tilo, exported, run


async def main() -> None:
    async with Tilo() as tilo:                             # your backend's key
        for customer in ["cus_8121", "cus_8122", "cus_8123"]:  # your own ids for them
            await run(
                tilo,
                {"path": "account/workspaces", "action": "create"},
                {"name": f"Applicant {customer}", "labels": {"customer_id": customer, "plan": "visa"}},
                {"idempotency-key": f"workspace-for-{customer}"},  # a retry makes no second one
            )

        labelled = (await run(
            tilo, {"path": "account/workspaces", "action": "list"}, {"labels": {"customer_id": "cus_8121"}}
        ))["result"]["workspaces"]
        jane = labelled[0]                                  # none when there is none
        print("cus_8121 is", jane["id"], jane.get("labels"))
        visa = (await run(
            tilo, {"path": "account/workspaces", "action": "list"}, {"labels": {"plan": "visa"}}
        ))["result"]["workspaces"]
        print("visa customers:", len(visa))
        usage = await tilo.read_account_resource(path="account/usage")
        print("usage rows:", len((usage.get("fields") or {}).get("subscription", {}).get("usage", {}).get("workspaces") or []))

        # Before a customer leaves: everything their workspace keeps, then delete it.
        workspace = tilo.workspace(jane["id"])
        whole = await exported(workspace)
        print("exported", len(whole["resources"]), "resources of", jane.get("labels", {}).get("customer_id"))
        for gone in visa:
            await run(
                tilo, {"path": "account/workspaces", "action": "delete"}, {"workspace": gone["id"]}
            )


asyncio.run(main())
```

```typescript example=customer-labels
import { Tilo, exported, run } from "tilo-sdk";

type WorkspaceInfo = { id: string; name: string; labels?: Record<string, string> };

const tilo = new Tilo();                                     // your backend's key
await Promise.all(                                           // all at once: hundreds is fine
  ["cus_8121", "cus_8122", "cus_8123"].map((customer) =>    // your own ids for them
    run(
      tilo,
      { path: "account/workspaces", action: "create" },
      { name: `Applicant ${customer}`, labels: { customer_id: customer, plan: "visa" } },
      { "idempotency-key": `workspace-for-${customer}` },     // a retry makes no second one
    ),
  ),
);

const labelled = await run(
  tilo,
  { path: "account/workspaces", action: "list" },
  { labels: { customer_id: "cus_8121" } },
);
const jane = (labelled.result as { workspaces: WorkspaceInfo[] }).workspaces[0]!;
console.log("cus_8121 is", jane.id, jane.labels);
const visaAnswer = await run(
  tilo,
  { path: "account/workspaces", action: "list" },
  { labels: { plan: "visa" } },
);
const visa = (visaAnswer.result as { workspaces: WorkspaceInfo[] }).workspaces;
console.log("visa customers:", visa.length);
const usage = await tilo.readAccountResource({ path: "account/usage" });
const subscription = (usage.fields ?? {}) as {
  subscription?: { usage?: { workspaces?: unknown[] } };
};
console.log("usage rows:", subscription.subscription?.usage?.workspaces?.length ?? 0);

// Before a customer leaves: everything their workspace keeps, then delete it.
const whole = await exported(tilo.workspace(jane.id));
console.log("exported", whole.resources.length, "resources of", jane.labels?.customer_id);
for (const gone of visa) {
  await run(tilo, { path: "account/workspaces", action: "delete" }, { workspace: gone.id });
}
```

An export is every resource of every app in the workspace, each read whole as `tilo_read` reads it, with the
workspace's listing and labels; a resource that does not read is named in `unread` with why. A page's resources
are read side by side and written in the order a walk finds them, and the document streams out as it is read, so
the server holds no more than the reads in flight however much a workspace holds. Exports of many workspaces run
side by side up to a bound; the rest wait their turn rather than fail. The files the
resources name are `tilo.content:` references: download each with `files.download` (`GET
/v1/workspaces/<workspace>/contents/<hash>`) while the workspace still exists. From a shell,
`tilo do account/workspaces#create name="Applicant cus_8121" labels='{"customer_id":"cus_8121"}'` and
`tilo do account/workspaces#list labels='{"customer_id":"cus_8121"}'` do the same.

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
(`pdf-viewer/application.pdf#fill_field`) and writes a cover letter in Writer (`word-processor#append_paragraph`,
then `#export_document` as docx). It cannot list, read or act in any other workspace: another workspace's
MCP URL answers its token `403`, every `/v1/workspaces/<other>/...` route answers `403 beyond_scope`, a
tool call naming another workspace reaches nothing, and the account's paths (`account/...` apart from the
keys its workspace admitted and its list of workspaces: usage, making, deleting, minting) are refused.
Listing `record:tilo.grants` names the key and the keys it minted there, each a record of the workspace's own. Listing workspaces
with it names its own alone. Revoking the key ends its open MCP sessions at once: their next call is
refused, and the token opens no new one (`401 key_revoked`). Every call that
changed a resource answers that resource's `link`, and `tilo_link {path}` answers the link to anything a path
names, so the agent can end its work by handing a person the link to what it did.

## Review

Open the workspace in Tilo, or invite the person who reviews it:

```sh
tilo do workspace/invitations#invite role=editor invitee_name=ops@example.com -w "Applicant: Jane Doe"
tilo read record:tilo.workspace:workspace/members -w "Applicant: Jane Doe"
tilo do workspace/invitations#list -w "Applicant: Jane Doe"
tilo do workspace/members#name name="Jane Doe" -w "Applicant: Jane Doe"   # what its people and your invitations call you
```

Following the link signs them in to Tilo (with a passkey, or by approving an agent) and lands them in the
workspace as that role. No mail is sent: hand the link over yourself. From then on, the resource links your
agent hands over open straight in the resource's app for them.

A resource link is the one a person copies in the app, for example
`https://tilo.sh/app/#/@eu1.tilo.sh/w/<workspace>/so.tilo.pdf-viewer/<record>`. It carries no secret: it
opens only for the workspace's members, and anyone else meets the workspace's admission, so invite the
reviewer first. Your key invites as you would, as far as your role in the workspace goes; an agent's key asks
you first unless you allowed it in the workspace's settings. The reviewer then works with a key of their own: their account lists the
workspace with the role they have there (`run(tilo, {"path": "account/workspaces", "action": "list"})`,
each row carrying `role`), so they reach it by name too.

Each agent session is one invocation in History, named after its key. Undo it to take back everything it
changed ([invocations and history](../tilo-sh/invocations-and-history.md)). Watch it live from your backend with
`workspace.events()` ([events](../tilo-sh/events.md)).

## When a customer leaves

Delete their workspace, and their data goes with it:

```sh
tilo do account/workspaces#delete workspace="Applicant: Jane Doe"   # final; every key that reached only it is revoked
```

Deleting is final: there is no undo, and nothing asks again, so the call is the confirmation. The host
removes the workspace's documents, files and history and stops counting them against your storage. Anyone
with it open is told it was deleted, and their Tilo forgets it; a desktop or browser that was away is told
when it is back, also one of someone invited to only that workspace. Every key minted for it is revoked:
its agent's next call is refused `key_revoked`, which says when. Only a key reaching
all your workspaces deletes, and only workspaces of your own; one you were invited into is `not_found`.

A key the host turns away is refused `key_revoked`, `key_expired` (a minted key past its expiry) or
`invalid_token` for one it never issued, each with HTTP status 401 ([errors](../tilo-sh/errors.md)).

## The same from a shell

```sh
tilo do account/workspaces#create name="Applicant: Jane Doe"
tilo do account/workspaces#branch workspace="Onboarding template" name="Applicant: Amir Haddad"   # or start from one set up once
tilo files put passport.png -w "Applicant: Jane Doe"
tilo -w "Applicant: Jane Doe" do pdf-viewer#open_source address=tilo.content:… name=application.pdf
tilo -w "Applicant: Jane Doe" do pdf-viewer/application.pdf#read_fields
tilo -w "Applicant: Jane Doe" do word-processor#export_document format=docx
tilo files get tilo.content:… -w "Applicant: Jane Doe" -o cover-letter.docx
tilo do account/keys#mint workspace="Applicant: Jane Doe" name="Jane Doe's agent"
tilo do workspace/invitations#invite role=editor invitee_name=ops@example.com -w "Applicant: Jane Doe"
tilo read record:tilo.workspace:workspace/members -w "Applicant: Jane Doe"
tilo do "account/keys/<grant>#revoke"
tilo do account/workspaces#delete workspace="Applicant: Jane Doe"   # when the customer leaves
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
