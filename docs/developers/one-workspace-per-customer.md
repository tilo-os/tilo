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
from tilo import Tilo                      # uv add tilo-sdk  (TypeScript: bun add tilo-sdk)

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

# The reviewer's link: it opens the filled form in PDF Viewer.
print(ws.link("pdf-viewer/application.pdf"))
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
agent gets: `tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status`
and `tilo` (tilolang). It fills the PDF's form fields
(`pdf-viewer/PDF Viewer#field-edit`) and writes a cover letter in Writer
(`word-processor#append-paragraph`, then `#export-content` as docx). It
cannot list, read or act in any other workspace. Every call that changed a
record answers that record's `link`, and `tilo_link {path}` answers the link
to anything a path names, so the agent can end its work by handing a person
the link to what it did.

## Review

Open the workspace in Tilo, or invite the person who reviews it:

```python
invitation = await ws.members.invite(role="editor", email="ops@example.com")
print(invitation.link)          # hand it over: no mail is sent yet
print(await ws.members.list())  # who is in the workspace, with their roles
```

Following the link signs them in to Tilo (with a passkey, or by approving an
agent) and lands them in the workspace as that role. From then on, the links
your agent hands over open straight in the record's app for them:

```python
async with ws.run("Fill Jane's application") as run:
    ...                          # the agent's calls
print(run.links)                 # the link to each record the run changed
print(run.link)                  # the one link, when it changed one record
print(await ws.link("pdf-viewer/application.pdf"))   # the link to any path
```

A record link is the one a person copies in the app (for example
`https://tilo.sh/app/#/@eu1.tilo.sh/w/<workspace>/so.tilo.pdf-viewer/<record>`).
It carries no secret: it opens only for the workspace's members, and anyone
else meets the workspace's admission, so invite the reviewer first. Your key invites into
the workspaces you administer, as an editor at most; a key minted for one
workspace invites nobody. The reviewer then works with a key of their own:
their account lists the workspace with the role they hold there
(`await reviewer.workspaces.list()`, each row carrying `role`), so they reach
it by name too (`reviewer.workspace("Applicant: Jane Doe")`).

Each agent session is one run in History, named after its key ("Jane Doe's
agent #1"). Undo a run to take back everything it changed. Watch it live from
your backend with `ws.watch()`.

## When a customer leaves

Delete their workspace, and their data goes with it:

```python
tilo.workspaces.delete("applicant-jane-doe")   # {"deleted": id, "revoked_keys": [...]}
```

Deleting is final: there is no undo, and nothing asks again, so the call is
the confirmation. The host removes the workspace's records, files and history
and stops counting them against your storage. Anyone with it open is told it
was deleted, and their Tilo forgets it; a desktop or browser that was away is
told when it is back, also one of someone invited to only that workspace. Every key minted for it is revoked: its
agent's next call raises `KeyRevoked` (`key_revoked` on the wire), which says
when. Only a key reaching all your workspaces deletes, and only workspaces of
your own; one you were invited into is `not_found`.

A key the door turns away says why: `KeyRevoked`, `KeyExpired` (a minted key
past its expiry, `key_expired`), or `KeyRefused` with `invalid_token` for one
it never issued. All three are a `ProtocolError` with HTTP status 401.

## The same from a shell

```sh
tilo account workspaces create "Applicant: Jane Doe"
tilo files put passport.png -w applicant-jane-doe
tilo -w applicant-jane-doe do pdf-viewer#read-form-fields
tilo -w applicant-jane-doe do word-processor#export-content format=docx
tilo files get tilo.content:… -w applicant-jane-doe -o cover-letter.docx
tilo keys mint -w applicant-jane-doe --name "Jane Doe's agent"
tilo members invite --role editor --email ops@example.com -w applicant-jane-doe
tilo members ls -w applicant-jane-doe
tilo keys revoke <grant>
tilo account workspaces rm applicant-jane-doe   # when the customer leaves
```

## What is underneath

There is no separate REST API. Making and deleting workspaces and minting keys
are actions on your account (`account/workspaces#create`,
`account/workspaces#delete`, `account/keys#mint`), inviting is the
workspace's own (`workspace/members#invite`), and everything in a workspace is
reached through the MCP tools at
`https://mcp.tilo.sh/w/<workspace>/mcp`. The SDKs ask for typed answers
([the wire](sdk-wire.md)), and `PUT /w/<workspace>/content` takes a file's
bytes. A minted key is a certificate your key signed, narrower than it. The
hosts and the runtime check it like any other device of yours, and it ends
when your key is revoked ([design](../architecture/workspace-api.md)).
