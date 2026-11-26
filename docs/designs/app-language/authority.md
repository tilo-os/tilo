# Authority and versions

## Who may do what

Decisions are made by Tilo's one policy (`crates/core/action-policy`): grants answer
**Allow**, **Ask** or **Deny** on the tree workspace › app › type › resource › item › field ›
action. A person's own gestures and their agent default to allowing reads and undoable changes,
and asking before anything that reaches outside.

tilolang adds one input: **who wrote the code.**

1. Every action a view or block can invoke carries the identity of the code that wrote it.
   Passing it into someone else's view doesn't change its author.
2. What runs is allowed only as far as both the person using it and every author of the code
   in between are allowed, checked at the moment it runs. If an author loses a permission,
   everything they wrote loses it at once.
3. Code may change its own types' resources freely. Anything else (another app's actions,
   other resources, reaching outside) needs a grant.
4. A stranger's code has no grants, so everything foreign it does asks first. An agent's code
   can do at most what the agent may do.
5. Keeping code (pinning a view your agent made) trusts that exact
   version, by hash. A new version asks again for anything it adds.
6. Asks are shown by the system, never by the code: the prompt shows the action itself, not the
   label the view gave it. History records the whole chain ("you, via Alex's view").

**What an ask shows.** An action says what the person asked to allow a call of it reads, in
order, with `shows:`: a parameter (`note`), a field of the record one takes (`d.subject`), or a
call of one of the app's own `fn`s on parameters, `invites(text)`, for what is in an argument but
not plain to read (who an invitation's text names). Each is shown as text unless it says how:
`(d.to, .addresses)`, `(d.body, .long)`, `(key, .secret)`. A call's row is labelled with the
function's name in words and holds what it answers for the call's arguments, a list one item to a
line, read where the ask is built, through the run that makes the call, as a field is. The
function only reads; the compiler checks it takes exactly the parameters it is given.

```
action take_invitation(c: Calendars, text: Text) -> Text (
    shows: [addresses_named(text), (organised_by(text), .addresses), (invites(text), .addresses)],
) { ... }
```

What one question shows is bounded and never hides that it is: its rows of addresses name 20
together at most, and a call naming more, or an address the person could misread (letters outside
plain ASCII, a quoted part), is refused with why before anyone is asked; a long value is shown by
its start, with a row saying how much follows.

**Code can ask who reads what.** `members(r)` lists the workspace's members who may read the
resource `r` now, and `std.may_read(who: Actor, r: host.Resource) -> Bool` answers it for one
member, a person or an agent: the answer a read by them would get, from the same grants, reader
rules and per-member privacy. Someone who is no member of the workspace, an automation or an app
reads nothing, and so does everyone where the host gave no members. Code that would copy what a
resource says to someone else (an ask to an agent mentioned in it) asks first, and tells an agent
that may not read it so instead of copying it. Both read the membership the host gives, so a view
that asked shows the new answer once a grant changes.

`members(r)` is also who belongs to `r`: a channel, a shared folder or any resource keeps no
membership list of its own. Code changes it through the workspace's own actions,
`workspace.grant_read(workspace.members, resource: r, member: p)` and `revoke_read`
(`use tilo.workspace as workspace`), admitted by the workspace's grants on granting and revoking.
Each person or role holds at most one reading grant on a resource, and a change that leaves them
where they would stand without it removes that grant instead of writing another.

The compiler computes which actions a piece of code can reach. Granting, keeping and updating
are decided against that list, shown in plain words, so nobody has to read code to decide.

**Agents may suggest.** `suggest act(args)` is the call itself, admitted at Ask: where the
authority would let it run, it asks the person all the same, and what it denies stays denied. The
call waits as its own asked record, answered only by the person on the system's confirm, never by
the agent that made it; the resource it is about shows that it waits and who suggested it.
Approving runs it once, as the call it was; declining changes nothing.

## Versions

- Every change records which package version made it.
- A workspace pins its apps' versions; updating is the workspace's decision.
- On update, the compiler re-checks everything that depends on what changed (views, calls
  between actions, kept answers, other apps) and lists what broke.
- Schema changes migrate. A migration never deletes data or changes an id; what can't be read
  is set aside, never dropped.
- A package migrates its own documents with a `migration action` on the type whose resources
  change. A field the type no longer uses stays declared as an ordinary field: its values stay
  in the resources, and migrations read them. The platform runs the migration once on each
  resource of its type, as an ordinary action in history (undone like any other), recording in
  the same change that it ran. It may move items between collections, which keeps their ids.
  A migration upgrades only what an earlier version wrote: a resource this version makes is
  recorded as already migrated in the change that makes it, and what it starts with is its
  type's `initial action`, never a migration.
- An app renamed keeps what it had: `app so.tilo.lang.tasks "Tasks" (previously:
  so.tilo.lang.todo)` reads the resources under the old id as its own, where they are. The old id
  is signed into the app, and only the same publisher's app can take it over. The renamed app
  keeps opening the workspace document of its earliest id
  (`<workspace>#app:so.tilo.lang.todo`): nothing is copied to a resource of the new id. It works
  on that resource as itself: its grants, publications and windows name the new id, and the session
  admits it there only once the takeover is verified.

## Not yet

Sharing views (code) between people: embedding, showing or adopting another person's view. It
needs its own design (presentation of untrusted code, adoption, updates) and isn't supported
until then. People share resources; each person sees them through their own views.
