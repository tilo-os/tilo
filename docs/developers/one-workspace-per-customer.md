# For your product: one workspace per customer

Your backend runs an agent for each of your customers. Give each customer a
Tilo workspace of their own: your agent works on their documents there, with
no access to anyone else's. Your people review its work in Tilo, see what the
agent did step by step, and undo any of it.

You need three things, all from one API key:

1. **A key for your backend.** It reaches all your workspaces, makes new ones
   and mints keys. You approve it once in your Tilo, and it lasts until you
   revoke it.
2. **A workspace per customer**, with their files in it.
3. **A key per customer's agent.** It reaches that one workspace and nothing
   else, expires within 90 days (or sooner, with `expires_in`), and stops
   when you revoke it or the key that minted it.

## Get a key

```sh
tilo keys create "Visa backend"     # prints a link: approve it in your Tilo
export TILO_API_KEY=tilo_sk_…       # what it prints once you approved
```

The key is shown once: keep it in your secret store. It works until you
revoke it, so your backend does not break when some date passes. To have it
expire anyway, say when: `tilo keys create "Nightly job" --expires-in 2592000`
(seconds).

You can also create a key on your grants page (`https://mcp.tilo.sh/grants`).
Keys are listed there with the agents you connected, with when each was last
used and which of your devices approved it. If a key leaks, revoke it there
with one click, or with `tilo keys revoke <grant>`: that ends every key it
minted too. The `tilo_sk_` prefix lets secret scanners find a key committed
by mistake.

## A workspace per customer, in Python

```python
from pathlib import Path
from tilo import Tilo                      # uv add tilo  (TypeScript: @tilo/sdk)

tilo = Tilo()                              # the key in TILO_API_KEY
for applicant in ["Jane Doe", "Amir Haddad", "Li Wei"]:   # one per customer, no limit
    ws = tilo.workspace(tilo.workspaces.create(f"Applicant: {applicant}").id)

    # Their files: the bytes go up once, then the apps take them by reference.
    ws.files.upload(Path(f"{applicant}/passport.png"))
    form = ws.files.upload(Path(f"{applicant}/application.pdf"))
    ws.do("pdf-viewer#open")
    ws.do("pdf-viewer#import-pdf-content", content=form, name="application.pdf")

    # A key for this applicant's agent: it reaches this workspace alone.
    agent = ws.keys.mint(name=f"{applicant}'s agent")
    hand_to_agent(agent.mcp_url, agent.token)   # your agent runner

# Later, for review: what the agent filled in, and the letter it wrote.
ws = tilo.workspace("applicant-jane-doe")
print(ws.do("pdf-viewer#read-form-fields"))
letter = ws.do("word-processor#export-content", format="docx")
Path("cover-letter.docx").write_bytes(ws.files.download(letter))
```

The same calls are `async` on `AsyncTilo`, and `tilo sdk gen --lang python -w
<workspace>` writes typed classes for the apps in a workspace
(`read_form_fields()` answers a typed `FormFields`), so an editor completes
them.

Hand the agent the MCP URL and token. Any MCP client connects with them. For
example, with Claude Code:

```sh
claude mcp add --transport http jane https://mcp.tilo.sh/w/<workspace>/mcp \
  --header "Authorization: Bearer <token>"
```

The agent sees that one workspace's apps through the same tools a person's
agent gets: `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status`
and `tilo` (tilolang). It fills the PDF's form fields
(`pdf-viewer/PDF Viewer#field-edit`) and writes a cover letter in Writer
(`word-processor#append-paragraph`, then `#export-content` as docx). It
cannot list, read or act in any other workspace.

## Review

Open the workspace in Tilo, or share it with the person who reviews it. Each
agent session is one run in History, named after its key ("Jane Doe's agent
#1"). Undo a run to take back everything it changed. Watch it live from your
backend with `ws.watch()`.

## The same from a shell

```sh
tilo account workspaces create "Applicant: Jane Doe"
tilo files put passport.png -w applicant-jane-doe
tilo -w applicant-jane-doe do pdf-viewer#read-form-fields
tilo -w applicant-jane-doe do word-processor#export-content format=docx
tilo files get tilo.content:… -w applicant-jane-doe -o cover-letter.docx
tilo keys mint -w applicant-jane-doe --name "Jane Doe's agent"
tilo keys revoke <grant>
```

## What is underneath

There is no separate REST API. Making workspaces and keys are actions on your
account (`account/workspaces#create`, `account/keys#mint`), and everything in
a workspace is reached through the MCP tools at
`https://mcp.tilo.sh/w/<workspace>/mcp`. The SDKs ask for typed answers
([the wire](sdk-wire.md)), and `PUT /w/<workspace>/content` takes a file's
bytes. A minted key is a certificate your key signed, narrower than it. The
hosts and the runtime check it like any other device of yours, and it ends
when your key is revoked ([design](../architecture/workspace-api.md)).
