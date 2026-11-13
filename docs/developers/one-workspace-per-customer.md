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

Hand the agent its MCP URL and token. Any MCP client connects with them. For example, with Claude Code:

```sh
claude mcp add --transport http jane https://mcp.tilo.sh/w/<workspace>/mcp \
  --header "Authorization: Bearer <token>"
```

The agent sees that one workspace's apps through the same tools a person's agent gets: `tilo_ls`,
`tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status` and `tilo`. It fills the PDF's form fields
(`pdf-viewer/application.pdf#field-edit`) and writes a cover letter in Writer (`word-processor#append-paragraph`,
then `#export-content` as docx). It cannot list, read or act in any other workspace. Every call that
changed a record answers that record's `link`, and `tilo_link {path}` answers the link to anything a path
names, so the agent can end its work by handing a person the link to what it did.

## Review

Open the workspace in Tilo, or invite the person who reviews it:

```sh
tilo members invite --role editor --email ops@example.com -w applicant-jane-doe
tilo members ls -w applicant-jane-doe
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

Making and deleting workspaces and minting keys are actions on your account
(`account/workspaces#create`, `account/workspaces#delete`, `account/keys#mint`), inviting is the
workspace's own (`workspace/members#invite`), and everything in a workspace is reached through the API at
`https://mcp.tilo.sh/w/<workspace>/`. A minted key is a certificate your key signed, narrower than it: the
hosts and the runtime check it like any other device of yours, and it ends when your key is revoked
([design](../architecture/workspace-api.md)).

There is no separate REST API, and nothing here needs one: the SDKs ask for typed answers over the same
calls an agent makes, and a file's bytes go up in one request before an app takes the reference
([files](../tilo-sh/files.md)).
