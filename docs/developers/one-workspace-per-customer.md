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
with jane.run("Write the cover letter"):
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
const letter = await jane.run("Write the cover letter", async () => {
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

```curl example=workspace-per-customer
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"                 # your backend's key
json="Content-Type: application/json"

for applicant in "Jane Doe" "Amir Haddad"; do               # one per customer, no limit
  workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "$json" \
    -d "{\"name\": \"Applicant: $applicant\"}" | jq -r .id)

  # Their files: the bytes go up once, then an app takes the reference.
  printf 'a passport scan, in bytes' | curl -sS --fail-with-body "$api/workspaces/$workspace/uploads" \
    -H "$auth" -H "Content-Type: application/octet-stream" --data-binary @- > /dev/null

  # A key for this applicant's agent: it reaches this workspace alone.
  key=$(curl -sS --fail-with-body "$api/keys" -H "$auth" -H "$json" \
    -d "{\"workspace\": \"$workspace\", \"name\": \"$applicant's agent\"}")
  echo "$applicant: MCP at $(jq -r .mcp_url <<<"$key")"
  [[ $applicant == "Jane Doe" ]] && jane=$workspace jane_key=$(jq -r .token <<<"$key")
done

# The customer's agent, with only its own key: it writes the cover letter, as one run.
agent="Authorization: Bearer $jane_key"
run=(-H "Tilo-Run: cover-letter" -H "Tilo-Run-Title: Write the cover letter")
word=$api/workspaces/$jane/resources/word-processor/actions
for text in "Dear Visa Officer," "My passport and application form are enclosed."; do
  curl -sS --fail-with-body "$word/append-paragraph" -H "$agent" -H "$json" "${run[@]}" \
    -d "{\"text\": \"$text\"}" > /dev/null
done
letter=$(curl -sS --fail-with-body "$word/export-content" -H "$agent" -H "$json" "${run[@]}" \
  -d '{"format": "docx"}' | jq -r '[.. | strings | select(startswith("tilo.content:"))][0]')

# The same key cannot reach anyone else, or make a workspace of its own.
curl -sS "$api/workspaces" -H "$agent" -H "$json" -d '{"name": "Applicant: someone else"}' \
  | jq -r '"the agent cannot make workspaces: \(.error.code)"'

# Later, for review: your backend takes the letter out and hands a person the link to it.
bytes=$(curl -sS --fail-with-body "$api/workspaces/$jane/content/$letter" -H "$auth" | wc -c)
link=$(curl -sS --fail-with-body "$api/workspaces/$jane/links?path=word-processor" -H "$auth" | jq -r .link)
echo "the letter is $bytes bytes; a person opens it at $link"
```

## Hundreds of customers: label, find, export

A backend serving many customers keeps its own id for each one on their workspace as a **label**, and finds
the workspace again by it: no table of workspace ids to keep beside your own database. Labels are names
(1 to 63 of `a-z`, `0-9`, `_`, `-` and `.`) to text values, at most 16 per workspace. Only your own keys
read them: nobody you invite into the workspace sees them, and a key minted for it never sets them.

Passing your customer's id as the create's idempotency key makes the create safe to repeat: asked again,
after a timeout or a crash, it answers the workspace the first one made instead of making a second.

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

// Before a customer leaves: everything their workspace holds, then delete it.
const exported = await tilo.workspace(jane.id).export();
console.log("exported", exported.resources.length, "resources of", exported.workspace.labels.customer_id);
for (const gone of await tilo.workspaces.list({ labels: { plan: "visa" } })) {
  await tilo.workspaces.delete(gone.id);
}
```

```curl example=customer-labels
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"                 # your backend's key
json="Content-Type: application/json"

for customer in cus_8121 cus_8122 cus_8123; do              # your own ids for them
  curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "$json" \
    -H "Idempotency-Key: workspace-for-$customer" \
    -d "{\"name\": \"Applicant $customer\", \"labels\": {\"customer_id\": \"$customer\", \"plan\": \"visa\"}}" \
    > /dev/null
done

jane=$(curl -sS --fail-with-body -G "$api/workspaces" -H "$auth" \
  --data-urlencode "label[customer_id]=cus_8121" | jq -r '.workspaces[0].id')
echo "cus_8121 is $jane"
curl -sS --fail-with-body -G "$api/workspaces" -H "$auth" --data-urlencode "label[plan]=visa" \
  | jq -r '"visa customers: \(.workspaces | length)"'

# Before a customer leaves: everything their workspace holds, then delete it.
curl -sS --fail-with-body "$api/workspaces/$jane/export" -H "$auth" \
  | jq -r '"exported \(.resources | length) resources of \(.workspace.labels.customer_id)"'
for gone in $(curl -sS --fail-with-body -G "$api/workspaces" -H "$auth" \
    --data-urlencode "label[plan]=visa" | jq -r '.workspaces[].id'); do
  curl -sS --fail-with-body -X DELETE "$api/workspaces/$gone" -H "$auth" > /dev/null
done
```

An export is every resource of every app in the workspace, each read whole as `tilo_read` reads it, with the
workspace's listing and labels; a resource that does not read is named in `unread` with why. The files the
resources name are `tilo.content:` references: download each with `files.download` (`GET
/v1/workspaces/<workspace>/content/<content>`) while the workspace still exists. From a shell,
`tilo account workspaces create "Applicant cus_8121" --label customer_id=cus_8121` and
`tilo account workspaces ls --label customer_id=cus_8121` do the same.

[`sdk/typescript/examples/workspace_per_customer.ts`](../../sdk/typescript/examples/workspace_per_customer.ts)
is all of this at a backend's scale: it makes 300 labelled workspaces at once, finds each by its customer
id, mints a key for each, has every customer's agent work over MCP at once with its own token (list, read,
an action, a file put), checks that no token reaches its neighbour's workspace over MCP or REST, revokes a
third of the keys, exports and deletes everything, and prints the timings, every failure with why, and the
host's memory and CPU. Against a local tilo.sh:

```sh
eval "$(os/containers/tilo-sh/stack.sh env)"                  # TILO_API_URL: the local API server
eval "$(head -1 ~/.local/state/tilo-sh-local/operator.env)"  # TILO_API_KEY: the stack operator's server key
CUSTOMERS=300 SAMPLE_PIDS=relay:$(cat ~/.local/state/tilo-sh-local/run/relay.pid) \
  bun sdk/typescript/examples/workspace_per_customer.ts
```

(`TILO_SH_STATE` moves the stack's state from `~/.local/state/tilo-sh-local`.)

## What the agent's key reaches

Hand the agent its MCP URL and token. Any MCP client connects with them. For example, with Claude Code:

```sh
claude mcp add --transport http jane https://mcp.tilo.sh/w/<workspace>/mcp \
  --header "Authorization: Bearer <token>"
```

The agent sees that one workspace's apps through the same tools a person's agent gets: `tilo_ls`,
`tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status` and `tilo`. It fills the PDF's form fields
(`pdf-viewer/application.pdf#field-edit`) and writes a cover letter in Writer (`word-processor#append-paragraph`,
then `#export-content` as docx). It cannot list, read or act in any other workspace: another workspace's
MCP URL answers its token `403`, every `/v1/workspaces/<other>/...` route answers `403 beyond_scope`, a
tool call naming another workspace reaches nothing, and the account's paths (`account/...`, `GET
/v1/account`, making, deleting, minting) are refused. Listing workspaces with it names its own alone. Revoking
the key ends its open MCP sessions at once: their next call is refused, and the token opens no new one
(`401 key_revoked`). Every call that
changed a record answers that record's `link`, and `tilo_link {path}` answers the link to anything a path
names, so the agent can end its work by handing a person the link to what it did.

## Review

Open the workspace in Tilo, or invite the person who reviews it:

```sh
tilo members invite --role editor --email ops@example.com -w applicant-jane-doe
tilo members ls -w applicant-jane-doe
tilo members name "Jane Doe" -w applicant-jane-doe   # what its people and your invitations call you
```

Following the link signs them in to Tilo (with a passkey, or by approving an agent) and lands them in the
workspace as that role. No mail is sent: hand the link over yourself. From then on, the record links your
agent hands over open straight in the record's app for them.

A record link is the one a person copies in the app, for example
`https://tilo.sh/app/#/@eu1.tilo.sh/w/<workspace>/so.tilo.pdf-viewer/<record>`. It carries no secret: it
opens only for the workspace's members, and anyone else meets the workspace's admission, so invite the
reviewer first. Your key invites into the workspaces you administer, as an editor at most; a key minted for
one workspace invites nobody. The reviewer then works with a key of their own: their account lists the
workspace with the role they hold there (`tilo.workspaces.list()`, each row carrying `role`), so they reach
it by name too.

Each agent session is one run in History, named after its key. Undo a run to take back everything it
changed ([runs and history](../tilo-sh/runs-and-history.md)). Watch it live from your backend with
`workspace.watch()` ([events](../tilo-sh/events.md)).

## When a customer leaves

Delete their workspace, and their data goes with it:

```sh
tilo account workspaces rm applicant-jane-doe   # {"deleted": id, "revoked_keys": [...]}
```

Deleting is final: there is no undo, and nothing asks again, so the call is the confirmation. The host
removes the workspace's records, files and history and stops counting them against your storage. Anyone
with it open is told it was deleted, and their Tilo forgets it; a desktop or browser that was away is told
when it is back, also one of someone invited to only that workspace. Every key minted for it is revoked:
its agent's next call raises `KeyRevoked` (`key_revoked` on the wire), which says when. Only a key reaching
all your workspaces deletes, and only workspaces of your own; one you were invited into is `not_found`.

A key the host turns away says why: `KeyRevoked`, `KeyExpired` (a minted key past its expiry,
`key_expired`), or `KeyRefused` with `invalid_token` for one it never issued. All three are a
`ProtocolError` with HTTP status 401 ([errors](../tilo-sh/errors.md)).

## The same from a shell

```sh
tilo account workspaces create "Applicant: Jane Doe"
tilo files put passport.png -w applicant-jane-doe
tilo -w applicant-jane-doe do pdf-viewer#import-pdf-content content=tilo.content:… name=application.pdf
tilo -w applicant-jane-doe do pdf-viewer/application.pdf#read-form-fields
tilo -w applicant-jane-doe do word-processor#export-content format=docx
tilo files get tilo.content:… -w applicant-jane-doe -o cover-letter.docx
tilo keys mint -w applicant-jane-doe --name "Jane Doe's agent"
tilo members invite --role editor --email ops@example.com -w applicant-jane-doe
tilo members ls -w applicant-jane-doe
tilo keys revoke <grant>
tilo account workspaces rm applicant-jane-doe   # when the customer leaves
```

## What is underneath

Making and deleting workspaces and minting keys are the account's
(`POST /v1/workspaces`, `DELETE /v1/workspaces/<workspace>`, `POST /v1/keys`), inviting is the
workspace's own (`POST /v1/workspaces/<workspace>/members/invitations`), and everything in a
workspace is a resource under `/v1/workspaces/<workspace>/resources/...`. A minted key is a
certificate your key signed, narrower than it: the
hosts and the runtime check it like any other device of yours, and it ends when your key is revoked
([design](../architecture/workspace-api.md)).

Every one of these is also an MCP call an agent makes (`account/workspaces#create`,
`account/keys#mint`, `workspace/members#invite`), answered typed: one API, two ways to say it
([`api.md`](api.md)). A file's bytes go up in one request before an app takes the reference
([files](../tilo-sh/files.md)).
