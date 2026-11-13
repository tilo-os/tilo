# Workspaces

A workspace is one place of its own: its apps, its files, the people in it, and everything that happened
in it. A backend usually wants one per customer, so no customer's documents sit in another's. A person
usually wants one per job.

## What is in one

| | What it is |
| :--- | :--- |
| Apps | Notes, Writer, PDF Viewer, Browser, Terminal, Mail. `ls` lists them, and each app lists what is in it |
| Resources | Everything an app holds: a note, a PDF, a window, a whole notebook. Each has a path and actions |
| Files | Bytes an action takes or makes, named by a `tilo.content:` reference ([files](files.md)) |
| People | You, and anyone you invite, each with a role |
| Keys | The keys that reach this workspace and nothing else ([keys](identities.md#keys)) |
| History | Every change, who made it, which run it was part of, and what can be undone |

## Naming a workspace

A workspace has an id (a UUID), which the SDKs and the API use, and a name you choose. You can name it by
either. Names match case-insensitively, and the `tilo` command matches their words:

| You wrote | It reaches |
| :--- | :--- |
| `tilo -w applicant-jane-doe do notes#new-note …` | the workspace named `Applicant: Jane Doe` |
| `tilo.workspace("Applicant: Jane Doe")` | the same workspace |
| `tilo.workspace("3f2a8c1e-0b4d-4e5f-9a6b-7c8d9e0f1a2b")` | the workspace with that id |

A command that names no workspace acts on this computer. With `-w`, it acts in the hosted workspace named,
using the key in `TILO_API_KEY`.

## Yours, and ones you were invited into

You are the **owner** of the workspaces you make. In one you were invited into, you hold the role you were
invited with: **viewer** reads, **commenter** also reacts and comments, **editor** also changes things,
and a **moderator** decides who else is in. Listing your workspaces says which is which.

Your agents are held to the same roles: an agent's key never makes the agent more than an editor in a
workspace. A key that reaches one workspace reaches no other, so a customer's agent cannot list, read or
act anywhere else.

## Making and deleting a workspace

Making one is an action on your account, like minting a key. So it appears in history, obeys permissions,
and can be done from the page, the command or the SDK:

```sh
tilo account workspaces create "Applicant: Jane Doe"
tilo account workspaces ls
```

Deleting a workspace is final. There is no undo and nothing asks again: the call is the confirmation.
The host removes its records, files and history, stops counting them against your storage, tells anyone
who has it open that it was deleted, and revokes every key that reached only it.

```sh
tilo account workspaces rm applicant-jane-doe   # {"deleted": …, "revoked_keys": [...]}
```

Only a key that reaches all your workspaces deletes, and only workspaces of your own: one you were
invited into, or one already deleted, answers `not_found` ([errors](errors.md)).

## People in a workspace

Inviting someone answers a link. Following it signs them in and lands them in the workspace as that role.
No mail is sent: you hand the link over.

```python example=workspace-members
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Hiring: Ada Lovelace").id)

invitation = workspace.members.invite(role="editor", email="reviewer@example.com")
print("invite link:", invitation.link)
print("sent by mail:", invitation.sent)

for member in workspace.members.list():
    print("member:", member.name or member.person, member.role)
```

```typescript example=workspace-members
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Hiring: Ada Lovelace")).id);

const invitation = await workspace.members.invite({ role: "editor", email: "reviewer@example.com" });
console.log("invite link:", invitation.link);
console.log("sent by mail:", invitation.sent);

for (const member of await workspace.members.list()) {
  console.log("member:", member.name || member.person, member.role);
}
```

```curl example=workspace-members
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Hiring: Ada Lovelace"}' | jq -r .id)
members=$api/workspaces/$workspace/members

invitation=$(curl -sS --fail-with-body "$members/invitations" -H "$auth" \
  -H "Content-Type: application/json" -d '{"role": "editor", "email": "reviewer@example.com"}')
echo "invite link: $(jq -r .link <<<"$invitation")"
echo "sent by mail: $(jq -r .sent <<<"$invitation")"

curl -sS --fail-with-body "$members" -H "$auth" \
  | jq -r '.members[] | "member: \(.name // .person) \(.role)"'
```

Only a key you created that reaches all your workspaces invites, and it invites into a workspace you
administer, as an editor at most. A key minted for one workspace invites nobody. Listing the members also
shows the invitations nobody has accepted yet. Once the person is in, your agent's record links open
straight in the record's app for them, because a link opens for the workspace's members and carries no
secret.

## Where next

- [Resources and actions](resources-and-actions.md): what is inside, and how to reach it.
- [Runs and history](runs-and-history.md): who did what, and taking it back.
