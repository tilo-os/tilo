# Audited typed state mutations

A resource can explicitly whitelist state-only actions with
`Resource::allows_low_mutation` and enables `Resource::AUDITED_LOW_MUTATIONS`.
Both default to false; unaudited resources keep their existing catalogue. The shared
`apply-low` and `group-low` programmatic actions decode each value and require
both this closed whitelist and an actual `Action::risk()` of `Low` before
encoding any mutation. This distinction matters because the action trait's
default risk is Low; it does not prove an action has been audited.

The target limits the JSON payload to 1 MiB and groups to 1–256 members. It
checks current `Resource::permission` for the decoded action, or the strongest
member permission for a group. Existing publication, acknowledged document
routes, broker admission, effect custody, per-author history and undo remain
required. A refused group applies nothing. It grants no file access or command
execution.

`Dispatcher::apply` selects this path only for an opted-in Low value. A group
uses it only when all its members select this path; the target revalidates every
member. The original generic `apply` and `group` retain their dynamic-risk tags
and confirmation behavior. Explicit `Invoke::apply` also preserves that path.

No resource opts in at present. The whitelist is a security review
obligation; callers cannot extend it.

Native public-action qualification and model coding evaluation are separate
receipts. A unit or mounted-app regression alone proves neither native
transport acceptance nor model quality.
