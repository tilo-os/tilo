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
| History | Every change, who made it, which group it was part of, and what can be undone |

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
tilo workspace create "Q2 Launch" --label team=product
tilo workspace ls --label team=product
```

Labels are yours, for your scripts and agents to find a workspace by.
A workspace keeps up to 16, each a name (`a-z`, `0-9`, `_`, `-`, `.`) and a text value. Only your keys read
them; nobody you invite in sees them.

Deleting a workspace is final. There is no undo and nothing asks again: the call is the confirmation.
The host removes its records, files and history, stops counting them against your storage, tells anyone
who has it open that it was deleted, and revokes every key that reached only it.

```sh
tilo workspace rm q2-launch   # {"deleted": …, "revoked_keys": [...]}
```

Only a key that reaches all your workspaces deletes, and only workspaces of your own: one you were
invited into, or one already deleted, answers `not_found` ([errors](errors.md)).

## Branch a workspace

A branch is a new workspace of your own that starts with everything one of yours holds: its records,
with their history, and the files they name. From then on the two go apart, and nothing merges back.
What is tied to people stays behind: the conversations in Chat, each person's own places, and the
members. You are the branch's only member. The source's labels stay behind too; give the branch its own. A
source that runs commands unattended passes that on: the branch starts with it on, for the keys made for
the branch alone ([approvals](approvals.md#commands-in-a-workspaces-machine)).

So set a workspace up once, then branch it for each task or each customer:

```sh
tilo workspace branch "Onboarding template" "Onboarding: Acme" --label customer_id=cus_8121
```

```python example=branch-workspace
from tilo import Tilo

tilo = Tilo()

# Set a workspace up once.
template = tilo.workspace(tilo.workspaces.create("Onboarding template").id)
template.do("snippets#new-snippet", title="Welcome letter")

# Branch it for a customer: the branch holds what the template holds.
branch = tilo.workspaces.branch(template, "Onboarding: Acme", labels={"customer_id": "cus_8121"})
acme = tilo.workspace(branch.id)
letter = acme.read("snippets/Welcome letter")
print("branch:", branch.name, "holds:", letter.fields["title"])

# Change the branch: the template stays as it was.
letter.favorite = True
assert acme.read("snippets/Welcome letter").fields["favorite"]
assert not template.read("snippets/Welcome letter").fields["favorite"]
print("the branch changed, the template did not")
```

```typescript example=branch-workspace
import assert from "node:assert";
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();

// Set a workspace up once.
const template = tilo.workspace((await tilo.workspaces.create("Onboarding template")).id);
await template.do("snippets#new-snippet", { title: "Welcome letter" });

// Branch it for a customer: the branch holds what the template holds.
const branch = await tilo.workspaces.branch(template, "Onboarding: Acme", {
  labels: { customer_id: "cus_8121" },
});
const acme = tilo.workspace(branch.id);
const letter = await acme.read("snippets/Welcome letter");
console.log("branch:", branch.name, "holds:", letter.fields.title);

// Change the branch: the template stays as it was.
await letter.edit(["favorite"], true);
assert.ok((await acme.read("snippets/Welcome letter")).fields.favorite);
assert.ok(!(await template.read("snippets/Welcome letter")).fields.favorite);
console.log("the branch changed, the template did not");
```

A workspace also branches itself: `workspace.branch("Onboarding: Acme")`. A key for the branch is minted
as for any workspace ([keys](identities.md)); it reaches the branch and not the workspace it came from.
Only a key that reaches all your workspaces branches, and only workspaces of your own: a key for one
workspace is refused with `not_permitted`, and one you were invited into answers `not_found`. A branch
that could not take everything is removed again, and the error (`branch_incomplete`) names what did not
come; the same call under the same idempotency key makes the branch once that is there. As with making
one, the same idempotency key answers the same branch.

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
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Hiring: Ada Lovelace"}' | jq -r .result.workspace.id)
tree=$api/workspaces/$workspace/resources/workspace

invitation=$(curl -sS --fail-with-body "$tree/invitations/actions/invite" -H "$auth" \
  -H "Content-Type: application/json" -d '{"role": "editor", "invitee_name": "reviewer@example.com"}')
echo "invite link: $(jq -r .result.link <<<"$invitation")"

curl -sS --fail-with-body "$tree/members/actions/list" -H "$auth" -X POST \
  | jq -r '.result[] | "member: \(.name // .member.person) \(.role)"'
```

A key you created that reaches all your workspaces invites as you would, as far as your role in the
workspace goes. An agent asks you first, unless you allowed it in the workspace's settings, under Agents. Tilo sends
no mail to the address an invitation names: hand the link over. `workspace/invitations#list` names the invitations
nobody has accepted yet. One you no longer want is cancelled with
`tilo workspace people revoke <invitation>` (`workspace/invitations#cancel {invite}`):
its link lets nobody in from then on. Once the person is in, your agent's record links open
straight in the record's app for them, because a link opens for the workspace's members and carries no
secret.

## Owners and administrators admit people

A workspace is kept for its **owners**. They, and the administrators they make, are the people who let
everyone else in, always through an action of the workspace: an administrator makes administrators, an
owner makes owners.

```sh
tilo workspace people add KEY --role editor --name "Ada Lovelace" -w Lounge   # in at once, no link to hand over
tilo workspace people invite --role editor -w Lounge                          # a link; whoever opens it joins
tilo workspace people wait-list -w Lounge                                     # who left an address at an entry
tilo workspace people invite --waiting ENTRY --role editor -w Lounge          # invite one of them
tilo workspace people unlist ENTRY -w Lounge                                  # take the address off again
```

`tilo workspace people add` takes a person's user public key (`tilo login --new-identity --json` names it), so it
lets someone in before any device of theirs joins. `--role administrator` makes another administrator.
A workspace has one owner, and `--role owner` hands it on, which only that owner does, from a device of
their own. `tilo workspace people invite` answers a link instead: `--email` names the address it is
for, and `--waiting` invites an address already on the wait list.

The same actions are the workspace's own resources, which an agent reaches over MCP (`tilo_do {"path":
"workspace/members#add", "args": {"person": KEY, "role": "editor"}}`, `workspace/invitations#invite`,
`workspace/waiting#list` and `#unlist`) and REST reaches as any action
(`POST /v1/workspaces/{workspace}/resources/workspace/members/actions/add`).

A deployment names its owner when it starts the host: `TILO_WORKSPACES_OWNERS` takes their user public
key, and the host hands the workspace to them at its first start, staying in it as an administrator
that keeps the deployment's settings. Everyone after them comes in through the actions above.

## Pausing the host

An operator sheds load without stopping a host by writing `spectator-only` or `back-soon` to the
`mode` file in the workspace's state directory; `normal` opens it again. The file outlives a
restart, so a host starts in the mode it was last left in. While paused the entry admits nobody new — a device of someone
already in the workspace enters as it does at any other time — and answers a newcomer a `503` asking
it to come back. `spectator-only` still serves the host's public directory listing; `back-soon`
stops that too, so nothing of the host answers a stranger. `GET /mode` on the deployment's own
listener says which mode is in force beside the host's state and revision, so a deployment can see
the switch took.

## Where next

- [Resources and actions](resources-and-actions.md): what is inside, and how to reach it.
- [Invocations and history](invocations-and-history.md): who did what, and taking it back.
