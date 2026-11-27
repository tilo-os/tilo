# One workspace

A workspace is the one unit of collaboration. It is the same thing on the
native desktop, on a workspace host, in the standalone runtime and in the
browser. There is no second unit beside or under it: no personal space, no
continuity path.

A workspace has three parts:

1. **Layout**: its strips, the window groups on them and the apps open in
   them.
2. **People**: its members, their roles and grants, and how someone is
   invited and admitted.
3. **State and history**: the resources its apps keep, its tasks and
   shortcuts, and the history of all of these.

A person has as many workspaces as they like, and every one of them is on
every one of their devices. None of them is special. A new workspace starts
empty.

## Document access

Workspace members hold a base role and can hold additional workspace-defined roles.
The most specific applicable rule decides. At the same document, a person's override
takes precedence over their custom roles, and those roles can extend access beyond
the default. Administrators can assign roles in **People** and edit document rules in
**Privacy & Security**.

Some records are each one member's: their watch history and their unsent message
drafts. The workspace policy decides them like any other record: no role reads or
changes them, owning the workspace does not reach them, and each member holds a
grant to their own, which the owners can widen. A member's view of the shared
layout is read by everyone, and only its member holds a grant to change it.
Shared channel drafts follow workspace roles. Earlier records with no trustworthy
owner stay retained without granting read access.

A restricted document uses its own MLS content key group. The group follows the
devices that can read that document. Delivery checks the same read permission before
replaying or forwarding document writes. When a device loses access, the group must
remove it before the updated membership can be used for new writes.

An authorized reader can still open older versions that were sealed with workspace
keys. A role change cannot take back copies a device already received or keys it
already held.

## Where app data lives

App data shared in a workspace resolves to the same records for every member.
Roles and document grants decide access to those records. Mail uses a workspace
mailbox and Passwords uses a workspace vault. What a host keeps beside a
workspace's records is the workspace's too: its mail accounts and their
sign-ins, its calendars, the vault's items, its model providers and their keys,
its MCP connections and what its agents learned sit in one store per workspace,
and the workspace's policy decides who reaches each (grants on Mail, Calendar,
Passwords, or the provider's own node), for people, agents and keys alike. Only
what is a person's own stays theirs on the host: how dates and numbers read to
them and what they are told about. A store a host kept per person before moves
into the workspaces that person owns when the host starts. Native Mail and Passwords provide actions to
recover the current member's earlier data into these shared resources. In
TiloLang Mail, **Recover previous mail** copies the current member's locally
held conversations, messages, labels and drafts into the shared mailbox in
batches. Conversation settings follow their messages into **Recovered mail**.
Drafts with known unsent jobs become editable drafts. A matching workspace
account with an empty signature can receive its archived signature. Recovery
writes only to the workspace mailbox.

Workspaces refuse new writes to archived per-member app documents. An app
with a workspace primary opens that same document for each member. The
archives retain their original read rules so members can recover earlier data.
Packages use shared workspace documents, or keep each member's records in
their own workspace (TiloLang's `resources: .own`). Recovery uses the locally
held copy for that person and workspace and marks unconfirmed outside effects
as unknown. The original archive remains available to its original reader.

What an agent learns about a person, in a turn of their own conversation with
it, is kept in the workspace's store, and each fact is read for the person it
is about alone. Signature profiles and saved TiloLang views live in the
person's store outside workspaces. Views retain their workspace
context so they can refer to its resources. Device-only data such as pending
local writes and granted file paths stays on that device.

## Members and devices

**A member is a person's identity**: a certified human root
(`crates/core/tilo-user-identity`). Membership, roles,
grants and history name members, never devices.

**A member has devices.** Each device has its own key, certified by the root;
the device key is its iroh `EndpointId`. Pairing brings the identity to a new
device; any of the person's devices can pair another, and the devices that hold
the identity key approve it. Revocations travel in the same hellos as
certificates and win over them. A removed device loses its future, not its
person's past: with the removal the workspace's governance keeps where each
resource's history stood for that device, and what it wrote up to there stays
valid for everyone, the devices added later too
([security](../security/architecture.md), 3.4). Each device lists the certificates it holds in
a document of the person's own store, which only their devices are handed, so
every device comes to know the others whichever of them it paired with; a
certificate read there is checked as one a device shows for itself. Nothing
of that store leaves a device for one it holds no such certificate of: a
device it never met is told who this one is, and catches up once the
certificate it shows is admitted. Another person's device is never admitted
through pairing; it comes in through an invitation to a workspace, as that
person's device.

**One way to add a device serves every device.** A machine is added with a
single-use, expiring ticket the person made for it. The ticket is signed by
the device that issued it and names, under that signature, where that device
is reached over iroh — its endpoint id and the packet relay that gets past a
network address translation — so a machine in a data centre is added as one at
home is. The new machine dials the issuing device, which asks the person to
approve; the approving device's root certifies the new machine's key over
exactly that key, and the answer carries the certificate the machine keeps.
Every image adds a device the same way. The new machine then enters the
person's own workspace at the server its ticket names, admitted there by the
certificate their root signed for its key: the list of their workspaces, and
where each of the others is kept, is read from the copy that brings. A device
the person's root never certified is refused by that workspace's own entry.

**Each device has a role in each workspace it holds:**

- **Key holder**: it holds the workspace's keys, reads and writes its state,
  and can do work in it.
- **Store**: it holds the workspace's encrypted data and serves it, and can
  read none of it.

One device can be a key holder in some workspaces and a store for others.
The two roles add up: a workspace can have any number of each.

**The role is chosen per workspace.** A member's own devices hold the keys of
the workspaces they are in. A server's role in a workspace is chosen by the
workspace's owners, and the server's owner agrees by offering it. A server of
tilo.sh holds the keys and takes work by default, because most people have no
device that is always on, and anyone may want more compute than their devices
have; a person who only wants their data kept and reachable makes it a store.
A person's own home server holds the keys of their workspaces by default, like
their laptop.

**Work in a workspace is the workspace's or a member's.** Workspace work is
what any key holder may do for everyone: applying actions placed on it,
answering search, a bot's schedule, an automation's steps within its grants.
A member's work acts with that member's authority beyond the workspace: an
agent using their mail or their signed-in Browser, an outbox entry that needs
their password. A key holder takes workspace work. It takes a member's work
only if it is that member's own device, or that member delegated the work to
it. So a server in a shared workspace answers everyone's search, but sends
mail only for the person who asked it to, and the other members' secrets
never reach it.

**Peer-to-peer comes first.** A workspace held by one person's PC is complete:
they invite someone, the invited device reaches the PC over iroh, and the two
work together with no server anywhere. Servers add reachability, copies and
compute; nothing needs one. Placement and leases are kept in the workspace's
own state, so a group of desktops, phones and servers that come and go shares
work by the same rules, and a device that goes away hands its work to the next
eligible one.

**Each device decides how much it wants to do.** A server may want to store
and sync every resource and file of every workspace it holds. A home server
or workstation may also take its person's work: run actions on their behalf,
answer search, run agents. A laptop or phone hydrates on demand. What a device
offers depends on the device itself (power, battery, load, capacity,
availability) and changes as those change. When several devices offer the
same work, placement picks among the eligible ones by operator priority, then
power (external, charging, unknown, battery), then load as a fraction of
capacity, then latency, and finally the device key, so equal candidates
always resolve alike; a passed-over device carries its reason.

**Work is leased, not owned.** State merges by itself; a side effect does not.
A piece of the workspace's work that must happen once — a bot's scheduled
trigger, an automation only one device should run — is taken under a lease kept
in the workspace's own state, so a group of desktops with no server has it. A
key holder that wants the work writes its own claim with a deadline it pushes
out while it works; claims rank by placement and the best live one of the
latest term holds. Two key holders that claim after a partition keep both
claims until the merge, then agree on one winner on every device; the loser
withdraws its claim and stops. A claim is over only a minute past its deadline,
so devices whose clocks differ by less than that never disagree about whether
the lease is free. What the work got to is workspace state too: a bot's
schedule keeps the fire time it answered, so whichever key holder holds the
lease carries on from it and a time missed while none ran is caught up once.

A server is not a different kind of node. It is a device with a large
appetite, usually always on, with the same software and the same protocol as
every other device. One server OS serves a Raspberry Pi at home and a data
centre holding thousands of people's workspaces; they differ only in what they
offer (storage, compute, machines) and, where a server holds several people's
workspaces, in keeping each workspace's work in its own sandbox.

**The hosts that hold a workspace.** A workspace lives on the devices that
hold it. The host whose root owns it holds it first; its
governance names every other host holding it beside that one, each by
an address that host signed. Being named there is what lets a host in
everywhere the workspace is held: its device holds the workspace, and its
person may reach the hosts that do. Another server of the owner's is also
admitted as one of the owner's devices. A server of another person,
such as tilo.sh, never becomes a member, so the workspace's members and roles
stay as they were; it holds the workspace as a store or as a key holder, as
chosen above. Of its own accord it writes one thing into the workspace: the
redemption of an invitation it answers. Every receiver checks that its device
was among the workspace's signed holders where it wrote the redemption, and
checks the invitation itself. As a key holder it also writes the work it
takes: workspace work placed on it, and a member's work under that member's
delegation, each signed so that history names the actor it acted for and the
server it ran on. Anything else it writes is refused. Adding people otherwise and changing roles stays with the owner's
and administrators' devices. Whom a workspace is held by is the
owner's decision: a device of the owner asks the host that serves it to have another
host hold it (`workspace.attach-host`, an operator's `hold`), and the serving
host hands the new one the workspace and names it. The other host takes it only when
its own operator's work setting takes that work: by default only its own
person's, and another person's only for the workspaces its operator names or
for anyone when it says so, so a server never holds strangers' workspaces
unasked. From then on both hold it: each keeps a writable copy and carries what is
written through it to every other holder, signed by the device that wrote
it, so its history and authors are the same on all of them, and each picks up
again after a restart. None of them is needed by the others: losing any one
leaves the workspace writable and served at the rest. The workspace's
compacted base stays with the host it was made on; the others publish only
their changes after it. A
member's device dials the holders alike, starting with the one it last
reached, and keeps its link with whichever answers. Every receiver reads a
holder's address only with that host's own signature on it, whoever wrote it
into the governance.
Detached, the other host lets the workspace go and keeps its files.

**How many copies a workspace keeps.** A workspace says how many key holders
and how many stores it wants, in its governance beside the servers that hold
it, chosen by the same owners (`copies.set`). One that never chose wants one
of each once a server holds it; a group of desktops with no server wants
nothing beyond the devices it has, so it needs no setup and is never short.
Each holder says in the workspace's state where its own copy stands: which
holders it is still taking what it lacks from, and which it cannot reach and
since when. A holder the owners take out takes its row with it: it is no
holder of the report any more, and nothing falls short about it.
Every device reads the same rows, so every device names the same
shortfall: which holder lacks what (history from another holder, or what was
written since it was last reached) and how many current copies of each kind
are missing (`tilo_peer_state::CopyStatus`, answered with the holder list). A
holder that has not said where its copy stands yet is not current: a copy
nobody heard of is not known to be whole, so it counts in no current copy
until its own row says it lacks nothing. A holder that comes back dials the
others, takes in what they hold and it lacks, and says it is current; the
others, reaching it again, say so too.
A store compacting its history keeps every frame a device that asked it for
history still lacks, and the newest checkpoint of each document.

### A machine copied from another

A person sets up one machine and branches others off it by copying its disk
(`tilo-vm vm branch`). The copy is a **new device of the same person**, never
a second instance of the first one: two machines with one device key would be
one iroh endpoint, one CRDT author and one filesystem subject.

The device key belongs to a machine. Beside it, `device-private/machine.bin`
records the firmware's system UUID (`/sys/class/dmi/id/product_uuid`), which a
VM host sets per machine and keeps across that machine's reboots;
`/etc/machine-id` is on the disk, so a copy carries it and it proves nothing.
At every boot, before the filesystem, the server, the network, SSH or a login
read anything, `tilo-device-branch.service` compares the two
(`tilo_user_identity::branch`):

- the recorded machine: nothing happens;
- nothing recorded (an install from before the document, or the first boot):
  the machine is recorded, nothing else. A copy of a machine that never booted
  a build with this unit cannot be told from its source this way, and both
  keep the one key: **boot the source once on such a build before branching
  it**;
- no firmware UUID (some hardware, any container): never re-keyed
  automatically;
- another machine: this disk is a branch, and it re-keys.

The re-key writes a root-only journal first (the keys the machine's stores may
be under, the new key, the machine it is for), and every later step can run
again, so a boot stopped anywhere resumes at the next one:

1. Every store sealed to the device key is re-sealed to the new key. Today that
   is each person's Secret Service vault, re-sealed by their own Secret
   Service running as them (`tilo-secret-service reseal-device`, keys on its
   standard input); root opens nothing in a home. Everything else a person
   keeps sealed — the accounts and passwords vault, model provider keys, Wi-Fi
   passwords, the MLS master key, the identity's own secret — has its key in
   that vault, so it opens as before. A vault under neither key (another
   machine's) is left untouched.
2. The new key replaces the device key.
3. The device is certified (below), and every certificate for the old key is
   removed from this machine.
4. A fresh `/etc/machine-id`; SSH host keys are removed and `sshd-keygen`
   makes new ones.
5. The machine is recorded and the journal, with the old key, deleted. A store
   that could not be re-sealed keeps the journal until a later boot re-seals
   it; the device acts with its new key meanwhile.

State that names the old device itself, rather than being sealed to its key,
cannot be re-sealed into the new one's, so it is dropped and rebuilt from the
device's peers:

- **Group membership.** Each MLS group the copy holds names the source's
  device as its member. The server's group member files (sealed under a key
  its device key derives) are dropped at re-key; a session's (sealed under the
  person's own vault) are dropped where the key service finds them naming
  another device (`key_group::Error::OtherDevice`). Neither is ever used,
  so the branch takes no leaf of its source's and cannot act as it. The
  branch then asks for each workspace's keys as itself, and a committer admits it
  with a `Welcome` and the key history, as for any further device of a member.
  The source stays the member it was.
- **Workspace membership.** The workspaces on the copied shell are held as the
  source's device: a workspace whose governance holds no certificate for this
  device is entered at its host as the person's further device — at the host
  the person's list names, or for one joined at someone's site, the host its
  own state names — and stays on the shell meanwhile, also when an entry does
  not land. The membership the host answers replaces the source's enrollment
  the copy held, which was never this device's to act with.
- **The person's own store.** The store the source kept is set aside beside it
  (`documents.of-<source>`), and the person's documents come back from their
  other devices.

The old device is **never revoked**: the machine it was copied from still is
it.

**What a branch keeps**: the person, everything on the disk, every secret, and
its workspaces. Membership names people, and a device certified by a member's
root is that member's device, admitted by any key holder when it first asks
(§ Membership is a resource), so no workspace has to invite the branch.
**What it gets new**: its device key (so its endpoint id, CRDT author and
filesystem subject), its certificate, its name — the source's name with
`branch` and four hex digits of the new key, renamed like any device — its
machine id and its SSH host keys. A setting such as accepting agents from the
person's other devices is on the disk and comes along.

**Certification adds no trust.** Only the person's root signs a device
certificate, and the root lives only in the person's identity store, so
nothing at boot signs:

- A person whose identity store holds the root (the usual primary device) has
  it issued when their identity service starts, the same provisioning that
  issues a certificate to any of their devices that holds none; it carries the
  branch's name (`device/branch-label`).
- A device that joined without the root holds nothing that signs. It waits,
  uncertified, for one approval: `tilo identity pair` on the branch (its name
  is the default) and `tilo identity approve` on a device that holds the root,
  as any personal device joins. Its secrets stay usable meanwhile: the stores
  are sealed to the device key, not to its certificate. Nothing lets a device
  key, or a copy of one, certify another key on its own: a stolen device key
  revoked from the person would otherwise mint unrevoked successors.

**A copy that kept the key is refused by name.** A copy this check cannot
tell from its source (above) keeps the source's key, and two machines then
sign two different changes under one sequence number. A receiver that sees
that refuses the key (`tilo_peer_state::CopiedDevice`: "<device> is running
on two machines; its changes clash. Re-add one of them with a ticket") and
takes nothing more under it until the person re-adds one of the machines with
a ticket: a device of theirs certified after the clash. On one machine, the
device's daemon is the key's one writer: it takes the key file's lock
(`tilo_device_identity::persist::take_device_key`), and a second process
taking it is told which process has it. A key the login Keychain or the
Android Keystore keeps is locked by a lock file beside its place, and a key
file on Windows by `LockFileEx`. Every other program of the computer that
needs the device to sign, seal or prove something asks the daemon's device
actions at its native endpoint (`tilo_device_identity::login::daemon`): the
Secret Service's vault and caller proofs, an image endorsement, a boot's
enrolment with the person's other devices, and a plain signature as the device
(`devices#sign`, which `tilo_identity::daemon_signer::DaemonSigner` turns into
a `DeviceSigner`). The daemon answers these for the device's own system and
the person's own programs on the computer, whose account could read the key
before, and never for an agent that runs elsewhere or in a roster turn. It
answers them as soon as it has taken the key, from the device's own actions,
before its person's services start or any workspace is admitted: a signature
depends on neither. The daemon's own parts sign with the key it took
(`persist::taken_device_key`) and never read it again. A program never gets an
endpoint secret from the daemon: only the daemon dials as the device. It gets back what the key made,
never the key, and with no daemon running it is told the device service
isn't running. Only first boot's `tilo-device-key-init` makes the key before
any daemon runs, and hands it over in the file it leaves.

### A person's servers

A person's servers are configured in their admin workspace: an ordinary
workspace whose only member is the person, in which each server, being one of
their devices, takes part, and which every one of their other devices holds
like any other workspace of theirs. Its id follows from the person's root, so
its devices find the same one; a server enrolled into the person's identity
makes it where it holds none and enters itself on its first start, and a
device of theirs that is not a server asks a server that holds it to take it
in. One document of kind `tilo.servers` holds each server's desired
configuration: its name, what it offers (storage, compute, machines), the
updates it follows and daily maintenance window, how it unlocks after a boot,
and whether it was retired. Every change is a typed action on that document, so
it has history and an author, and a device, an agent or a script change it the
same way, and a server applies what its entry says: no setting of a server
lives in a file of its own. Which workspaces a server holds, and as what (store
or key holder), is each workspace's governance alone: its holders list names
the server, and the workspace's own actions add, move and remove it. The
document keeps only what governance cannot name yet, the workspaces a planned
machine is to hold, which a device that hosts each one hands the machine once it
enrolled. A retired server is taken out of every workspace that names it.
What the person asks of a server is the same document and the same actions:
each entry keeps the operation asked of it (its health, a service's journal,
a restart, a reboot, an update, pausing or resuming updates, a rollback or a
drain) and what it answered. The server the entry names carries the request
out and keeps the result; the action that asked answers with it, so an
agent's `tilo_do` and a script reach a headless server the way they reach
anything else, and another person's device and agent reach none of it.
Neither an ssh session nor a second protocol has any part in it.
A server applies its own entry's offer to what it answers devices that offer
it work, and writes the updates it follows and its maintenance window for a
root-run helper to apply. A retired server lets go of everything it holds and takes no
work. Holding a workspace as an encrypted store, rather than as a key holder,
is not applied yet.

The same entry names the machine itself: its hostname, which drives it uses for
what — the system disk, the data pool, a mirror — by serial or WWN rather than
by kernel name, and how each network interface is set up (DHCP or static
addresses, a default gateway for each address family, DNS, a VLAN tag,
membership in a bond). These are typed
actions on the document like any other, so they carry history and an author,
and they refuse what a machine cannot be: a static interface with no address, a
second gateway of one family (a dual-stack link names one of each),
a second system disk, the disk the system runs from repurposed, a drive holding
data used without the person naming its serial to confirm erasing it. The
device writes the whole desired configuration as a request for a root-run
helper, which applies the hostname and the network as NetworkManager keyfiles
and checks each drive's identity before anything is written; it reports what it
applied or why not, and the entry's status carries that report back to the
person. A network change keeps the keyfiles it replaced until the device is
reached through a relay again, and puts them back with the reason when it is
not, so a wrong address does not lock the person out of their server. Disk
encryption, partitioning and the data pool's mount are the server image's own
boot chain, which this configuration names and verifies but does not replace.
The configuration is a device's, not a server's, so a desktop's entry carries the same
configuration.

## Transport

How devices carry a workspace's changes, today and in the one design that
survives, is in [the core](core.md), section 3.

## State

**All state is a CRDT-backed resource.** That includes settings and
preferences. An app's state is per (app, workspace): opening an app in a
workspace opens that workspace's state of it, and an app opened in a new
workspace starts empty. Within it an app keeps typed resources
(`#[derive(Model)]`, [`resources.md`](resources.md)) with stable ids; a
resource that outgrows one document is its own document, because an Automerge
change graph cannot be served in part.

**A resource exists once; workspaces reference it.** Bringing a resource into
a workspace adds a reference and moves no bytes; edits through either
workspace are edits to the one document. The reference is a resource of the
system's own kind in the workspace's store, naming the resource's workspace,
app and id; it grants nothing. The resource opens where it lives, for whoever
may read it there, and a member who may not sees the reference as unavailable.
Removing the reference leaves the resource as it is. Moving a resource to
another workspace is the store's `move`. "Duplicate here" is a different verb: it forks a new
resource with a provenance link. Bringing something in is a search result:
search lists resources from the person's other workspaces, labelled with
where they live, and activating one brings it here.

**A file is a resource whose identity is a path.** History, sharing and
permissions have one implementation, and the file case is not privileged.

### The folder holds files

The workspace's folder (its Files document, the tree every member mounts)
holds files and nothing else. App records such as notes or issues are not
shown there as files: an agent or a script reaches them through the app's
actions (`tilo ls`, `tilo read`, `tilo do`, or the same over MCP), and a
Markdown file put in the folder stays a file, which Notes can open.

## People

Membership and permissions are workspace state, changed by actions like any
other state and checked by the action policy
(`crates/core/action-policy`). A key holder with the grant admits a
member or a device and hands over the keys; there is no single device that
owns the workspace's governance.

**Late joiners get the full history.** A member or device added later reads
all of the workspace's existing state and history. Content keys are separate
from the keys of any one membership epoch; a joiner receives the past keys
with its admission and loads older history on demand, without any other
member being online. Removal stops future access; it cannot recall what was
already shared.

### Membership is a resource

A workspace's members, their roles, their grants and the devices each member
has in it are one resource of the workspace, `workspace.members`. Inviting,
admitting, changing a role, granting, removing and leaving are its actions.
Each is a signed op: its author's device key, certified by the author's root,
signs it, and the op names the ops it was written on (its causal heads), as
every op does.

Every key holder replays the resource and checks every op under the action
policy against the membership **as the op's author saw it**: the state at the
op's own heads, not the receiver's current state. An op its author was not
granted at that point is kept, so replicas stay byte-identical, but has no
effect. No device decides for the others; every key holder reaches the same
answer from the same ops.

**Administrators and the owner.** An administrator is a member at the
Editor role holding the administrator grants: every governance and moderation
action of the workspace, still bounded by that role. A workspace has one
owner, whom its governance names. Its owner hands it to another person with a
transfer, which only the owner makes and which every key holder replays in
order like any other op, so every device reaches the same owner and the same
keys whenever the transfer reached it. The owner it replaces stays in it as an
administrator. A host that makes a workspace for a person (tilo.sh, a
workspace of someone's own, one a person asks a host for) founds it and hands
it to them in the same step: the host stays a member and an administrator,
which is how it serves the workspace (it admits whom invitations name, names
its own address, writes checkpoints and relays what members write), and the
owner withdraws that like any administrator's. The policy then decides, for
every key holder alike:

- the owner and every administrator make administrators;
- only the owner unmakes an administrator, changes their role or removes,
  bans or restricts them, signed on a device of the owner's own: a host
  serving the workspace as an administrator does not sign it for them. An
  administrator never does it to another, and anyone gives up their own
  administration or leaves;
- an invitation can make its holder an administrator (an Editor's invitation
  in its own signed layout). Only someone who may make administrators issues
  one, and every key holder admitting its holder checks again that its
  issuer, and the person a host signed it for, may make them; one that fails
  admits nobody.

**Concurrent changes merge by one rule.** Ops that do not depend on each
other are applied in a fixed order: the author's role first (owner, then
administrator, then the rest), then the op id. Each op is checked at its place
in that order, so a change that a concurrent, earlier-ordered op made
unauthorised has no effect. Two administrators removing each other: the one
ordered first wins, and the other's removal comes from someone no longer
granted. A role change racing a removal of the same member: the removal is
kept if it orders first, the role change is moot if it orders later.
Concurrent edits that do not conflict (two people invited at once) both
apply.

**The log starts from what the workspace already held.** A workspace made
before the log has its membership in the document's own rows; its owner's
device writes them in as the first op, a genesis naming the membership those
rows held and the heads it was read at. Every receiver checks the heads are
history it holds and the author is the workspace's owner, so nobody can take
the membership back to an earlier one. A genesis by anyone else's device, or
naming a history a receiver does not hold, starts nothing.

The rows stay, as a projection: every admission writes them from the ops it
holds, and refuses a batch that changes a row the ops do not change. An older
build, a store and a server that keep only the document therefore read the
same membership every device computes, and the log is what decides it. A
batch may only add ops; a held op is never changed or dropped.

**Admitting.** An invitation is a signed grant naming the role it gives and,
when narrowed, the person it is for. Redeeming it writes the receipt, with
its signed invitation, into the workspace's rows; every key holder folds the
receipts it holds into the membership, checking the invitation at the joining
time the workspace documents for that person, so the invitation is judged once
and every key holder judges it the same. Any key holder that holds the
invitation admits it, with no particular device needed online. A member's own
further devices need no invitation: a device certified by a member's root is
that member's device, and any key holder admits it when it first asks. Which
role the device takes in the workspace (key holder or store) is the member's
choice, recorded in the same resource.

### Keys follow membership

Content is sealed under content keys (`key-group`), one per key epoch.
The key epoch's group is an MLS group whose members are exactly the
workspace's key-holding devices: a projection of `workspace.members`, never a
second document of who belongs.

- **Any key holder commits.** When the membership resource admits a
  key-holding device or removes one, any key holder that sees the group
  differ from the projection writes the MLS commit that brings it in line:
  `Add` with a `Welcome` and the bundle of every past content key for an
  admitted device, `Remove` and a fresh key epoch for a removed one. A
  key holder checks every commit it receives against the projection at the
  commit's place and refuses one that adds or removes anyone the resource
  does not.
- **One commit per epoch.** MLS orders commits linearly. When two
  key holders commit on the same epoch, every device keeps the commit with
  the lower hash; the other committer applies it and, if the group still
  differs from the projection, commits again on top. Commits only enact what
  the resource decided, so a lost race loses nothing.
- **Removal does not wait for a commit.** From the op that removes a member,
  every key holder refuses that member's further ops, whatever key epoch
  they are sealed under. The commit that rotates the key comes from the next
  key holder online, and protects content written from then on.
- **Late joiners read everything.** The bundle in an admission carries the
  key of every past epoch, sealed to the joiner's first epoch, so a joiner
  reads the whole history locally. It grows with membership changes, never
  with content.

**Workspace and document keys.** Shared documents can seal under the workspace
MLS group. A restricted document seals under its own group. Passwords vault
entries and Mail's workspace records use document keys from their first write.
The host checks the same read grant for document delivery and key admission.
Members with current read access can receive the document's earlier epoch keys.
Revocation stops new delivery and key admission for that member and rotates the
document group for subsequent writes.

**One genesis, made by the founder.** The workspace's governance names the
device that created it (`founder`). That device makes the group's genesis;
every other device waits, asks, and joins by the Welcome that admits it, and
no answer from a host makes it found a group of its own. If two geneses ever
meet, they reconcile like two commits: every device keeps the group whose
genesis has the lower digest, and a device in the other starts over and joins
the winner by Welcome, whose bundle brings the keys its writes were sealed
under. A document's cleartext writes stay the base its sealed writes are
changes over: a device joining after the seal takes that base in from the
host's replay, while a live cleartext write is refused.

A store never enters the group and never holds a key. It keeps and forwards
sealed ops, and refuses ones that are not signed by a member's device. To
tell, it reads the one part of `workspace.members` that is not sealed: the
device keys of the current members (no names, roles or grants), carried in
the signed envelope of each membership op.

## Layout

**Layout across devices.** Each device writes only its own namespace of the
workspace's `contents`. Rearranging a window another device owns is a request
to that owner (`tilo_desktop_model::commands::DesktopCommand`), never a write
into its namespace. The request is workspace state: it is kept while the
owner is offline, applied once when the owner next holds the workspace, and
recorded in the owner's namespace so a restart neither drops nor repeats it.
Following someone (focus, scroll position) is presence, not state.

What one device decided for itself stays on that device: which strip and group
are focused, widths, maximized groups, outputs, local file paths. That is the
device's desktop, never a second copy of the workspace.

**Continuing on another device.** Each device offers what it has in front —
the app, the document it shows and the view it shows it through — in the
record of the person's devices, the same record their notifications, their
session and their wakes travel on; a device the person takes the offer on,
or puts it away on, answers it there
(`tilo_workspace_service::workspaces::person_workspace::devices`). Only the
device whose window it is writes its offer, and only a device's person its
front. The person's other devices hold the record already, so they offer to
continue there and open what it named, with no channel of their own. What a
window resumes beyond that (an app's own small, path-free resume state) is
the app's, and the offer never holds a surface id, local path, file contents
or credentials.

**Sharing a window.** A window of an outside program (a terminal, any Wayland
or X client) keeps its state in its process, so its device streams that window
alone to the members viewing it, end-to-end encrypted, only while someone
views it. A Tilo app's window is a view of a resource the workspace already
holds, so sharing it shares the resource and nothing is streamed: other
members open the same resource in the same app. Driving someone else's window
needs its owner's control grant.

An app says which of the two its windows are. What a window shows may live in
the app's own process on the device that opened it, as a Browser's page lives
in its engine: its manifest says so (`[entrypoint] window = "device"`), and no
session code names the app. The device that opened such a window writes it
into the workspace's layout as a window that runs on it. Every other device
holds it as that device's window: it starts no instance of the app for it,
asking for the app there opens that device's own window, and the window's tab
shows its picture while its device shares it. An app that says nothing has
windows that are views of documents, and a window one device opens is one every
other device opens for itself.

A person shares a window from its own menu, "Share with workspace", on the
device that has it, and stops there. The row is one more caller of the
session's window share (`session:window-share`, `share` and `stop` by the
tab): it reads whether the window is shared, how many devices asked to see
it and whether it goes out to them from that resource, and keeps no state of
its own. Only a window the device maps, in a tab it minted, in a workspace
its session takes part in offers the row; a device that shows the window's
picture, or its placeholder, does not.

The share itself is workspace state. The device the window runs on writes the
share's grants, on the window's own node, into the workspace's governance as
its member's change (`WorkspaceMembershipCommand::Share`), so they leave with
the rest of what that device writes there. Every device that holds the
workspace, the sharer's included, reads them from the governance it admitted
and shows the window from there. Which device shares makes no difference to
who is shown it. A device that takes part through the workspace's host sends
the change to that host, which admits it and hands it on to the workspace's
other devices. The device that hosts the workspace keeps its governance
itself, so it has no host to send to: what it commits there (a share, its end,
a member's role, a grant, a device it admits) goes at once to the devices
linked to it in that workspace, as its document writes do, and to no device of
another workspace it hosts. A device that links later reads it in the
governance its link starts with. The admission matters to a share as much as
the share does: a device that was never told of a newcomer takes no commit
that brings the newcomer into the workspace's key group, and opens nothing its
host seals under the keys that follow. A shell takes the grants into its own
copy of a workspace only when the device holds no governance of that
workspace: one that lives on that shell alone, which has no other device to
tell.

Every other device shows the shared window as a window that runs on the
sharing device, and asks that device for its pictures. Where a shell already
lists the window as one that runs on that device, it is pictured in that tab,
and the tab stays when the share ends: the window still runs there, and its
tab is the layout's. Otherwise it stands in a tab of its own. Where a shell
lists the window under the window's own id as an app's window, that tab is the
device's own view of it, which it opened itself: it is left as it is, and the
shared window stands beside it in a tab the viewing device mints. That tab is
the viewing shell's alone, and never goes into the shell's record: a window that
runs on another device is that device's to add there. The two are different
things. The view is what the viewing device opened; the shared window is what
runs on the sharer's device, which is what someone reviewing it has to see.
When the share ends the shared window's tab goes, and the view stays. The
share's reading names the tab on the row of the window it watches
(`shown_in`).

A tab whose window runs on another device never stands empty. While a share
stands in it, it shows the share: the card of something arriving until the
first picture or page is here, then the window. Otherwise it shows the
toolkit's notice, titled with the app and the device the window runs on (a
device nothing names is another device of the workspace, never its key), and
under it why the window is not here, which the device's session reads: the
window is not shared with the workspace; it is shared and still on its way,
with what its share's reading says it waits for; nothing is heard of its
device in the workspace; or the workspace no longer admits that device. A
share this device let go says that it ended and why, and shows no picture
that no longer follows the window. The shares' reading says the same of each
such window in one word (`hosted`). Nothing here asks the window's device to
share it: a person shares a window on the device that has it.

A window whose app draws its own page, as the Browser's does, is shared as
that page and not as a picture of it. Its app hands the session the page as a
display list while a member's shell shows the window, the list travels the
live session on a track of its own, sealed as the pictures are, and the device
showing the window draws it with its own text and vector stack. Nothing of
such a window is captured or encoded, and the device showing it sends it no
input. Pictures of it would show a watcher what the page masks, so it is never
shared as pictures, also not for a while: the session knows that an app draws
its windows' pages from the app's package (`[entrypoint] window = "device"`
with `webview` among the capabilities it requires) and from the moment the app
says so on any of its connections, which is before it knows which window
speaks there. A share of such a window that is asked for before its page is
offered waits and says so (`starting`, `awaits_page`); a window of an app this
device holds no package of, shared as pictures before its app spoke, stops
going out as pictures in the pass the app does, and is the page's from then
on. The page goes out in the workspace the window is shared with, whatever
workspace its app was started in. The tile that draws the page names the page's text, each run where it
is drawn, so a reader who cannot see the page, and a tool that lists what a
shell shows, read what is on the screen.

The window's pictures travel the workspace's live session, which the device
serving the workspace relays for its members. A device is in the live session once
it holds the workspace's keys and reached that device; while it is not taken
in, it asks again, waiting longer each time and at most every fifteen
seconds, each device up to a quarter sooner by its own key so that the
devices a relay missed together do not come back together (`retry_wait` in
`crates/desktop/session/src/live.rs`), since a device that comes back tells
nobody. The relaying device takes sixteen sessions through admission at
once, at most four of them of one endpoint, and four more of endpoints that
none of its workspaces names as a device, which therefore cannot keep a
workspace's devices out (`MAX_ADMITTING`, `MAX_ADMITTING_OF_ONE` and
`MAX_ADMITTING_STRANGERS` in `crates/core/tilo-live/src/relay.rs`). One
more it closes as busy, unread: it judged nobody, so the device it closed
asks again the same way and reads as told busy by it (`relay-busy`), where a
device that did not reach it reads as waiting to reach it and a device it
judged and did not admit reads as refused. A live session reads thirty-two devices
at once (`MAX_HEARD` in `crates/core/tilo-live/src/live_session.rs`): each is
three tracks that it and the relay hold open, about 250 KiB between them,
and that, not the transport, is what a further device costs. A device that
is in the live session beyond those is not heard on this device, and the live session's
row says how many those are and why (`unheard`, `unheard_why`, `not_heard`).
The connection to the relay is allowed the streams thirty-two devices take
once its session is admitted (`MAX_BI_STREAMS` and `MAX_UNI_STREAMS` in
`crates/core/tilo-live/src/native.rs`), on a session's endpoint as on one
bound for the live session alone. The share's reading
says how far that came, since a live session that is not open drops nothing and
reports nothing by itself: `live` names each workspace's live session as open or by
what it waits for (the keys, the device that relays it, that device being
busy, or its refusal), a window shared or watched under a live session that is not open says the
same on its own row (`waiting_for`), and a shared window says whether it goes
out to a member's shell now (`streaming`): a captured window as soon as a shell
asks for it, a window shared as its page once its app handed a page over. A
stream's start puts nothing on a screen, so a device that shows a window says
whether its tile holds it (`pictured`: a decoded picture, or the page's whole
frame), and until then that its row waits for the first one; a shared window
a member shows, nothing of which went out, says that it waits too. A device
that asked to see a window is not yet one that sees it, so what the sharing
device tells its person follows `streaming` and nothing else: while nothing
of a window goes out, the bar's sharing pill, the window's menu and what both
tell a screen reader say that its viewers wait, and they say that someone
watches a window, or is in control of it, only while it goes out. `streaming`
says what left the sharing device; whether a viewer's own tile holds the
window is that device's to say (`pictured`), and it says so in the turn the
tile takes the window or loses it: a picture is decoded off the loop and taken
into the tile while the shell builds a frame, so the session asks its tiles
again once that frame is drawn. An open
live session also says the epoch it seals under, what it opened and could not open
of the other devices, and which of its windows another device asks it for
(`asked_for`), which is where two devices that hear nothing of each other read
why. A device that holds none of the workspace's keys and is lent the keys of
its own streams by the workspace's host, as the session in a workspace's VM
is, is in the live session on those (`lent_keys`): it sends what it shares and opens
nothing another device sends, and a window it watches says so.

## Presence

Presence is per resource, layout is per person. Inside a resource several
people have open, each sees the others' caret, selection, typing, pointer and
who is in it. Under "That I'm here" nothing else leaves the device: not which
windows or apps someone has, their desktop pointer, whom they follow, or what
the machine infers (an automatic status, idle or away). "Nothing" hides them
everywhere, inside resources too.

## History

A workspace's history is two levels: the outer level is actions on the
workspace itself (references added and removed, members joined and left,
grants changed, layout), the inner level is each resource's own history. The
workspace timeline shows a resource's edits as roll-ups ("Alice made 12
changes to Chapter 3") that expand into the resource's history, and filters
mean the same at both levels. Undo, Activity and Replay read those two levels;
nothing keeps a third log of the same changes.

## Branching a workspace

A person sets a workspace up once — a research environment, a development
workspace for a product — and branches it: one branch per question, one per
customer. A branch is a **new workspace of the person's own** that starts with
everything the source holds and goes on apart from it. It never merges back;
code comes back through git, not through Tilo.

One verb makes it, on the shell's chrome beside `new-workspace`:
`desktop:chrome` `branch-workspace` {`source`, `name`}, where `source` is the
workspace's name, its decimal id or the id it is shared under. The shell,
agents and automations use that action; the workspace menu and search offer it
as "Branch workspace", and the CLI is `tilo do branch-workspace`.
Tools that spell the workspace verbs `workspace.create` and `workspace.branch`
on the control socket reach the same two actions. The action answers the new
workspace's id at once; the device's daemon makes the branch
(`account/workspaces#branch`, the workspace service's `workspace.branch-own`),
all or nothing, while the shell goes on, and the branch goes on the shell — with the person in it — once
what it holds is there, so no app opens in it before its documents are and founds
an empty document of its own. The person is told when it is ready, and what did
not come along if anything did not.

**Who may branch.** An owner or current member can branch the workspace from a
device that holds it
(a workspace the device holds no governance of is the shell's own, and the
person's unless it names someone else). The branch copies what that device may
read. This includes the person's own drafts and watch history, still readable
only by them. Retained records without a verified owner stay unreadable in the
source. A branch made by a host leaves members' private records with their
devices. An agent may branch
only the workspace it works in, and only as the person's grants let it take
the shell's actions; a branch of another workspace is refused at the MCP server.

**What a branch copies**

- **Resources, with their history.** Every document the source holds is read
  whole and written into the branch as its founding write,
  sealed under the branch's own keys. Each document is forked under a
  writer of its own and gets one more change naming the branch as its
  workspace; its document id stays (an id names a document only inside its
  workspace, so references between the workspace's documents keep working) and
  so does where it was minted. No document is shared between the two
  workspaces: an edit in either stays there, also after a restart.
- **The workspace's files**, with the content their records name, copied into
  the branch's content by the store that keeps it.
- **Terminal environments.** The runtime that keeps them branches them
  (`ush`'s `branch-workspace`, asked by the daemon inside the branch): each document forked with its history, naming the
  branch as its workspace, its home and its grants of the source's files moved
  to the same places in the branch's, and what it keeps outside its document (its
  `/var` and legacy home) copied. Its id (`TILO_ENVIRONMENT`, `<branch>/<key>`)
  and its `TILO_WORKSPACE` are the branch's, so its sessions and its MCP server
  act in the branch only. File actions not carried out yet stay with the
  source. Images are the device's and shared already.
- **The shell.** The source's strips, a window for each of this device's
  windows in it (opened again by its app, in the branch), and what the
  workspace grants its apps, its own files named as the branch's. The branch's
  shared shell document is written from these windows, never copied from the
  source's document of everyone's.
- Secrets (the Secret Service, the passwords vault, provider keys) are the
  person's on the device, not the workspace's: a branch on the same device has
  them already.

**What stays behind** — what is tied to other people:

- Chat: every conversation, with each person's drafts and read marks, and
  the workspace's moderation reports and decisions. The workspace's emoji
  and agents come along.
- Each person's own place in the workspace's tilolang apps, one document for all
  of them.
- Membership and governance: the branch's are new — a new workspace id, its
  only member the person who branched it, as owner, its own keys. No
  other member, invitation, pending invitation or key state carries over.
- Other people's windows, who is here (presence), and the source's history of
  effects and its shortcuts.

The source is read and never changed.

**A workspace a host keeps.** A host that keeps workspaces of people's own
(tilo.sh) branches one of them as the host operation `workspace.branch-own`
{`workspace`, `name`, `branch`?, `labels`?}, beside `workspace.create-own`; the
API offers it as the account action `account/workspaces#branch`. Only the person who owns the source
branches it, from a device of theirs or a key reaching all their workspaces; a
key for one workspace makes none, and someone else's workspace is not theirs
to branch. The host is a member of every channel of the workspaces it serves,
so it copies what it reads, as a device does:

- every document of the source, read whole with its history, forked as above
  and founded in the branch as the host founds a document, sealed under the
  branch's own keys where its documents are sealed; a document the host does not
  read whole stops the branch before anything is made, and a person's own
  documents sealed for their devices alone (drafts) are not the host's to read;
- the content those documents name, and the chunks of the files that is, copied
  from the source's content store into the branch's under the same addresses,
  so the references they hold open in the branch.

What stays behind is the same rule as on a device
(`tilo_desktop_model::branch::stays`), and the branch's governance is new, its
only member the person, as its owner. The labels are the branch's own, given
with it; the source's stay with the source. The call answers once the branch
holds everything; if any part does not come, the branch is deleted again and
the call fails naming that part (`branch_incomplete`; why stays in the host's log), and it is listed among the person's
workspaces only once it is whole. `branch` is the new workspace's id: asked
again under the same one, the host answers the branch it made and copies
nothing twice (the API passes the id its `Idempotency-Key` names). Whoever keeps
more of a workspace than its documents hears of each branch made
(`Workspaces::branches`: source, branch, person), as of a deletion.

## The shell

The shell's surfaces (launcher, top bar, overview, quick settings,
notifications, onboarding, sheets and dialogs) are apps on exactly the same
toolkit and resource/action/view model as every other app. The workspace,
its layout and the shell's own state are resources like any other. The
compositor stays thin: windows, input, outputs and scanout. Rendering
capabilities the shell needs (retained layers, damage tracking, interactive
animation) belong in the toolkit, so every app has them.

## Resources that outgrow a snapshot

Chat keeps its messages in a chain of `ChatPart` documents
(`tilo_app::framework::chain`), because one document outgrows what a snapshot
can carry: a snapshot folds a document only up to 16 MiB, and a fold always
carries the document's whole history.

The chain stays in the SDK, as one resource. Segmenting one document's history
in the storage layer would need what Automerge does not have (a snapshot
without the history before it) and a compaction checkpoint every device
agrees on, and it would not help: a chat's current state grows with it, so a
newcomer would still load all of it. Every layer below already treats a
document as the unit and serves whole documents on demand.

So the chain's consumers see one resource and never parts: `Chain::publish`
presents every held part as one resource, and the framework, not the app,
holds an earlier part when a reader scrolls back to it. Headless readers
(bots, moderation, the Discord bridge, agents) read and page the chain through
that resource instead of enumerating parts themselves.

**A change costs its part, not its document.** Every holder of a document (the
relay, each device, the page) loads, verifies, diffs and saves the document
whole, so whatever grows without end in one document makes every change cost
everything before it. Anything an app keeps that grows without end is a chain
of bounded parts, and the document the app shows keeps only what stays small.
What one change costs on any holder is then the cost of one part, whatever
the workspace holds. The framework gives every chain the same pieces, so an
app writes none of them:

- **Where a write goes.** `Chained::part_for` names the part an action
  changes (the one holding the item it names; for a new item the head, or the
  next part once the head is full). `Chained::prepare` reads the app's document
  and every held part where the action is invoked, and fills in or refuses
  what spans parts (an issue's parent in another part, its place in the
  order), so the verb itself reads only its part and replays the same
  everywhere.
- **What one change means elsewhere.** `Chained::spread` names further
  actions the same invocation writes into other parts right after the action
  lands: renaming a note relabels the links to it in every part holding one.
  Each is the same invoker's step in its own part's history, so each part's
  change costs that part.
- **Held whole or by the reader.** A chain whose views list all of it
  (`Chained::HELD_WHOLE`: a tracker's issues, a reader's articles) holds every
  part as soon as it knows the head; one read back in time (a chat) holds the
  head and what a reader scrolled to. What one step changes across many parts
  (marking every subscription read) is written as a summary in the part it is
  invoked on, so the step costs one part too.
- **Read as it changes.** `ViewCx::chain_value` follows the document and the
  chain's parts the way `ViewCx::value` follows the document, and a part is a
  document a view part is built over (`Chain::part_document` with
  `ViewCx::document`), so an item's page binds its fields by path as before.
- **Published, searched and undone per part.** The chain's publication and
  search entries are built again only for the part that changed; its items
  open where the window shows them; the window's Undo takes back the
  person's last write into a part before the document's own history.
- **Held where a host acts.** A host acting on a document for a member or an
  agent hands the document it mounts the newest part of each of its chains and
  every part the chain then asks for, from what the workspace holds, and
  keeps each current with the changes after it.
- **Beside the mailbox.** Mail keeps folders, drafts and queues in the workspace mailbox.
  Its message-state chain uses independently sealed parts, with access determined by
  workspace grants. Earlier member mailboxes and their parts keep their original access
  during import.
- **Places over everything the chain holds.** What the app derives from all
  of it — a book's groups, its favorites, its likely duplicates — the chain
  publishes as its own children (`Chained::places`), and what search finds
  beyond the items (`Chained::search_over`), so derived resources do not
  need the document to hold the items.
- **A group as one step.** Several actions invoked as one (a saved form's
  field edits) land as one change in each part they touch, so they stay one
  undo step and one version.
- **A verb over items of several parts.** A verb naming many items (archive
  these messages) says what share of it each part holding some of them
  carries out (`Chained::split`); the shares are written into those parts
  only, as one step the window's Undo takes back whole.
- **What the outside is owed.** Items an outside server keeps too (a
  mailbox's messages, a calendar's events) carry what that server last
  confirmed beside what the person did. The work still owed is derived from
  each part (`ResourceEnv::owed`), read again only for a part that changed,
  and carried out on the device that holds the account; the app confirms what
  the server now holds with an action on the item's part. No queue is kept,
  so a change writes its one part and a confirmation writes only the part
  holding its item.

What one app document holds at the size of a small company's workspace (101 wiki
pages, about 2,500 issues, a 19,513-file repository), and what a change costs
there before it moves into parts:

| App | What one document holds | Grows with | One change today |
| --- | --- | --- | --- |
| Tasks | (before) every issue with its body, thread, links and attachment references | issues | 6.7 ms to apply and a 711 ms load at 2,500 issues (311 KB saved, 2.6 MB with history), every holder |
| Tasks | (now) projects and areas; issues in parts of at most 500 issues or 512 KiB of text | parts | one part's cost: the same bytes and document size at 10× the issues |
| Notes | (before) every page, its rich-text body and reactions | pages | 600 KB at 101 pages; whole document per change |
| Notes | (now) tags; pages in parts of at most 200 pages or 512 KiB of text | parts | one part's cost: the same bytes at 10× the pages |
| Files (folder) | (before) every file and folder node of the workspace | files | 28 MB at 19,513 files; whole document per change |
| Files (folder) | (now) the tree's own document, listing chunks of up to 1,024 nodes each | chunks | one chunk's cost: about 1 ms and 755 bytes sent per file written at 20,000 files |
| Mail | message state, held bodies, drafts, outboxes and saved copies | messages | grows with the mailbox |
| Feeds | (before) every article with its full text | articles | grows until its feed is removed |
| Feeds | (now) folders and subscriptions; articles in parts of at most 300 articles or 1 MiB of their text | parts | one part's cost, the same bytes at 10× the articles |
| Messages | every conversation with every message | messages | grows without bound |
| Discord bridge | a mapping row per mirrored message | messages | grows without bound |
| Calendar | every event with overrides and attendees | events | grows with the calendar |
| Contacts | (now) nothing; contacts in parts of at most 500 contacts or 512 KiB of text and portraits | parts | one part's cost: the same bytes and document size at 10× the contacts |
| Snippets, Photos, Music | every item; Music playlist covers inline up to 256 KiB | items | grows with the library |
| Chat | channels and people; messages in parts of 2,000 messages or 1 MiB | parts | one part |
| Sheets, Slides, Word processor, Canvas | one document per file | the file | bounded by the document |
| Desktop layout | strips and windows, and `effect_receipts` with no cap | receipts | grows with use |
| Settings, Shortcuts, Activity | small | — | small |

Binary content (attachments, images, file contents) is already a
content-addressed blob that the document only names (`Bytes` above 1 KiB, the
folder's iroh-blobs hashes), except Music's playlist covers and Contacts'
portraits, which are inline data.

A workspace's folder is split another way: its tree outgrows a document by its
files, not by a history that only grows at one end, and a file anywhere in it
is written again. So the tree is chunks (`fs_protocol::tree`). The tree's own document holds the
root's identity and lists the chunks; each chunk is a document of its own
(`derived:chunk.<n>@` the tree's document) holding up to about a thousand nodes
with their versions, attributes and leases. A node lives in the chunk its
document id names, chosen when it is made (the newest chunk while it has room),
and never moves, so renaming, moving, editing or deleting a file is a change
of that one chunk. Writing a file then costs what its chunk costs, whatever
the folder holds: one change sent, one chunk's journal appended, one chunk
compacted and sealed when its history fills, and one chunk read again by a
page or a host that follows the folder. A computer's folder sends what a burst
of writes changed as one write per chunk; who wrote each file is in its
version, not in how the writes were grouped.

## What a unit and a workspace's keys cost

Measured, not estimated: `key-group`'s `per_group` bench for a
workspace's group, `tilo-workspace-service`'s `record_cost` for a unit and its
document, and `tilolang-tilo`'s `index_units` for the index. Numbers are medians on a
x86_64 development container, three samples; instruction counts repeat on any
machine, times are that container's.

**One group per document** (one MLS group per document, three devices in each),
per document — what a group per document would cost:

| documents | join | per document | join bytes | removal | removal bytes | device state |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 1.99 ms | 1.99 ms | 3.1 KiB | 0.84 ms | 700 B | 757 B |
| 10 | 22.0 ms | 2.20 ms | 3.1 KiB | 0.77 ms | 700 B | 763 B |
| 100 | 197 ms | 1.97 ms | 3.1 KiB | 0.75 ms | 700 B | 760 B |
| 1000 | 1989 ms | 1.99 ms | 3.1 KiB | 0.77 ms | 700 B | 760 B |

The cost is flat per group and does not shrink as a workspace grows: 2.0 ms
and 3.1 KiB of governance frames per group to admit one device, 0.8 ms and
700 B per group for one removal, and 760 B of sealed peer state per group on
the device. A user joining a workspace of 1000 documents would pay about two
seconds and 3 MiB before reading anything, and every membership change after
that would repeat it, on every device.

**One group per workspace** (one MLS group carrying every unit of the
workspace, three devices), by the units it carries:

| units | join | per unit | join bytes | removal | removal bytes |
| --- | --- | --- | --- | --- | --- |
| 1 | 2.02 ms | 2.02 ms | 3.1 KiB | 0.97 ms | 700 B |
| 10 | 1.95 ms | 195 us | 3.1 KiB | 0.77 ms | 700 B |
| 100 | 1.87 ms | 18.7 us | 3.1 KiB | 0.74 ms | 700 B |
| 1000 | 1.88 ms | 1.9 us | 3.1 KiB | 0.76 ms | 700 B |

Flat in the units carried: a workspace of 1000 documents costs 1.9 ms and 3.1
KiB to admit a device and 0.76 ms and 700 B to remove one, instead of 1.99 s
and 3.1 MiB and 0.77 s and 700 KiB. Each unit's content seals and opens under
the workspace's keys, bound to the unit's artifact: 4.8 us per unit at one
unit, 2.4 us at 1000 (`unit_content`).

**One storage unit** (one document of the workspace's store: its changes file,
the index's snapshot of it and its rows), per unit:

| units | create | open | files | bytes | memory held open |
| --- | --- | --- | --- | --- | --- |
| 1 | 611 us | 49 us | 2 | 191 B | 15.9 KiB |
| 10 | 194 us | 201 us | 2 | 191 B | 15.9 KiB |
| 100 | 231 us | 45 us | 2 | 191 B | 15.5 KiB |
| 1000 | 205 us | 8.2 us | 2 | 191 B | 15.6 KiB |

A unit is cheap to have: about 200 us to make (its founding write, its file and
its index snapshot), 8 us to open at a thousand documents, two small files
totalling 191 bytes, and about 16 KiB of memory while it is held in the store.
Of those bytes, 120 are the document and 71 its index snapshot before it holds
a resource. Per-unit fixed cost is what keeps units small; what a unit costs to
read is its document.

**One document**, one document of small resources, one change per resource:

| items | snapshot | history | save | load | memory after load | one change | appended |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1k | 11.5 KiB | 181 KiB | 0.67 ms | 4.9 ms | 466 KiB | 0.26 ms | 141 B |
| 10k | 116 KiB | 1.80 MiB | 8.5 ms | 74 ms | 4.6 MiB | 0.93 ms | 142 B |
| 100k | 1.14 MiB | 18.3 MiB | 107 ms | 801 ms | 41 MiB | 14 ms | 142 B |

Loading a document costs about 5 to 8 microseconds per resource and holds about
36 to 40 times its snapshot in memory (the change graph, kept for merging). Its
history is about sixteen times the snapshot, and one more change is about 140
bytes whatever the size.

A document on disk also keeps its index's snapshot in `index/<id>`: every
resource's indexed fields and the heads the snapshot was taken at, which a start
reads instead of the document. It is about six times the document's own
snapshot — 68 KiB beside 12 KiB at a thousand resources, 689 KiB beside 116 KiB
at ten thousand, 6.7 MiB beside 1.1 MiB at a hundred thousand — and it is
written whole whenever the document changes: a change appends about 170 bytes
to the document's file but takes 3 ms at a thousand resources, 41 ms at ten
thousand and 573 ms at a hundred thousand, nearly all of it the snapshot. A
document the store let go of memory and a write or a read needs is loaded whole
and indexed again, which costs 66 ms at a thousand resources, 750 ms at ten
thousand and 9.2 s at a hundred thousand.

**One index**, one redb file per package and workspace, one row set per record:

| documents | write one document | open | one lookup | file |
| --- | --- | --- | --- | --- |
| 1 | 42 us | 0.04 ms | 5.1 us | 1.0 MiB |
| 10 | 30 us | 0.04 ms | 0.7 us | 1.0 MiB |
| 100 | 28 us | 0.06 ms | 0.5 us | 1.0 MiB |
| 1000 | 36 us | 0.12 ms | 0.6 us | 1.0 MiB |
| 10000 | 36 us | 0.10 ms | 0.6 us | 2.1 MiB |

An index answers a lookup in under a microsecond and opens in a tenth of a
millisecond whether it holds ten documents or ten thousand; the rows are about
100 bytes per document. What it costs per workspace and not per document is redb's
minimum file: about a megabyte before it has a document in it, which is why an
index is one per (package, workspace) pair rather than one per unit.

**A host opening a workspace** (`tilolang-tilo`'s `bench` example,
`--open-only`, p50 of three to five runs, taken on a sixteen-thread laptop while
other builds shared it, load average around fifty, so the times are inflated
several-fold; notes here, the board earlier, on a quieter aarch64 laptop):

| items | open (over the index) | of which building the view | load the document |
| --- | --- | --- | --- |
| 10k | 0.39 s (66 ms earlier) | 0.31 s | 0.45 s (412 ms earlier) |
| 100k | 3.6 s (1.04 s earlier) | 3.1 s | 4.3 s (6.7 s earlier) |

In both measurements loading the document costs as much as opening over the index
or several times more, so the index saves reading it but not having it: what a
host pays for a workspace is dominated by the document's own bytes and by the
view building its rows from them, and both grow with the number of items. A
chunk the views do not read in full is what brings either down.

What that implies for the layers above:

- **Keys per workspace, not per document.** Per-record keys would make a
  membership change linear in the workspace: the 1.96 ms above is one commit
  and one delivered frame set per group, on every member. One group per
  workspace makes the same change one commit, and a device's sealed state one
  file per workspace instead of 760 B per document.
- **A chunk is a few thousand items, not a document per app.** A 10k-item chunk
  loads in 31 ms and holds 3 MiB; a 1k-item chunk in 3.6 ms and 276 KiB. A
  chunk that holds a whole collection of a few thousand small resources keeps
  open, save and memory at tens of megabytes for a workspace of a hundred
  thousand items, where one document per app at that size is 27 MiB and
  325 ms to load before anything is shown.
- **Units stay separate for what is written separately.** A document's fixed
  cost is small enough that units are not the thing to merge; what has to
  change is what is written and sealed together, so that a chunk's commit is
  one scope and one append.

## Where the code differs

[The core](core.md) lists every copy of a core piece still on `main`, what
survives and what is deleted.
