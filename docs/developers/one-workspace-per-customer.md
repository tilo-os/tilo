# For your product: one workspace per customer

Your backend runs an agent for each of your customers. Give each customer a
Tilo workspace of their own: your agent works on their documents there, with
no access to anyone else's. Your people review its work in Tilo, see what the
agent did step by step, and undo any of it.

You need three things, all from one API key:

1. **A key for your backend.** It reaches all your workspaces, makes new ones
   and mints keys. You approve it once in your Tilo.
2. **A workspace per customer**, with their files in it.
3. **A key per customer's agent.** It reaches that one workspace and nothing
   else, ends when your key ends or sooner, and stops when you revoke it.

## Get a key

```sh
tilo keys create "Visa backend"     # prints a link: approve it in your Tilo
export TILO_API_KEY=tilo_sk_…       # what it prints once you approved
```

You can also create a key on your grants page (`https://mcp.tilo.sh/grants`).
Keys are listed there with the agents you connected, and revoking one ends
every key it minted.

## A workspace per customer, in Python

```python
from pathlib import Path
from tilo import Tilo                      # uv add tilo  (TypeScript: @tilo/sdk)

tilo = Tilo()                              # the key in TILO_API_KEY
applicant = tilo.workspaces.create("Applicant: Jane Doe")
ws = tilo.workspace(applicant.id)

# Their files: the bytes go up once, then the apps take them by reference.
passport = ws.files.upload(Path("passport.png").read_bytes())
form = ws.files.upload(Path("application.pdf").read_bytes())
ws.do("pdf-viewer#open")
ws.do("pdf-viewer#import-pdf-content", content=form, name="application.pdf")

# A key for this applicant's agent: it reaches this workspace alone.
agent = ws.keys.mint(name="Jane Doe's agent")
print(agent.mcp_url, agent.token)          # hand both to the applicant's agent

# Later: what the agent filled in, to review.
print(ws.do("pdf-viewer#read-form-fields"))
```

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
