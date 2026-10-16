# Local invocation locus basis

`ResourceInvocationScope::locus_basis()` exposes an optional SDK observation
captured when a bound resource invocation resolves its target, before encoding
or execution. It preserves the original invocation, actor and host-set
participant; the original published address; the resolved component-local
address and instance; the local owner lifetime and checkout; the admitted
record route; and the sorted committed document heads at that point.

The original alias is resolved by the SDK's live publication registry when the
scope is created. Capture compares the actual dispatch target against that
retained resolution and checks the scope's exact live record loan. A matching
resource name or participant ID alone cannot create a basis.

The basis is local metadata, not a text anchor, completed-action receipt,
successful-presentation receipt or wire authentication. It makes no claim that
the action succeeded, that the resulting document is visible, or that an active
frame existed. A synchronous invocation can finish before any such frame.

## Reading the route

Read route identity through the returned basis rather than decoding its
publication alias or interpreting the local checkout as a collaboration ID:

```rust,ignore
if let Some(basis) = scope.locus_basis()? {
    let route = basis.route();
    let app = &route.app_id;
    let room = route.cluster_id;
    let wire_record = route.record.as_deref();
    let local_checkout = basis.checkout();
    let original_address = basis.publication();
    let component_address = basis.target();
    let committed_heads = basis.heads();
}
```

`route.record == None` retains an app-primary route; it must not be replaced
with the local checkout's internal key. The owner lifetime and instance are
local to this SDK host. Mapping another authenticated publisher's route to a
receiving replica still requires separate custody evidence.

## Lifetime and limits

Reads recheck the original caller and owner lifetime. A returned copy is inert
historical data and is not reusable authorization. Cloned scopes share at most
one captured basis; later dispatch cannot refresh its frontier. Identity
conflicts make it unavailable. A pending edit produces no basis and observation
never calls `get_heads()` to commit it. Capture retains at most 128 heads and
16 KiB of identity/frontier payload, with at most 4 KiB per identity field.
Unqualified or oversized observations remain absent without changing action
output. Legacy/unbound scopes have no qualified component target and currently
produce no basis.

Future Writer integration must obtain an invocation-specific anchor from the
executing owner, qualify it against the appropriate document frontier, and
resolve it through the actual retained presentation. Writer's participant-only
outcome queue is not invocation proof. Document heads, owner lifetimes and GPU
or layout generations describe different facts; no new synthetic generation
or active interval is introduced here. No transport schema changes are included.
