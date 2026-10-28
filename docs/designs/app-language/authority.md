# Authority and versions

## Who may do what

Decisions are made by Tilo's one policy (`crates/collab/action-policy`): grants answer
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

The compiler computes which actions a piece of code can reach. Granting, keeping and updating
are decided against that list, shown in plain words, so nobody has to read code to decide.

## Versions

- Every change records which package version made it.
- A workspace pins its apps' versions; updating is the workspace's decision.
- On update, the compiler re-checks everything that depends on what changed (views, flows,
  kept answers, other apps) and lists what broke.
- Schema changes migrate. A migration never deletes data or changes an id; what can't be read
  is set aside, never dropped.

## Not yet

Sharing views (code) between people: embedding, showing or adopting another person's view. It
needs its own design (presentation of untrusted code, adoption, updates) and isn't supported
until then. People share resources; each person sees them through their own views.
