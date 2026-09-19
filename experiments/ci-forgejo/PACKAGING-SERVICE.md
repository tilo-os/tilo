# Managed qualification invocation (disabled staging)

The platform owns one invocation; developers and agents do not request runner
slots, select a packaging baseline, or run a local coordinator loop. The existing
runner scheduler remains responsible for capacity. No service or permissions are
activated by this change.

```sh
python3 -I tools/ci_python.py tools/app_packaging_service.py \
  --state /state/coordinator.sqlite \
  --policy /etc/packaging/coordinator.json \
  --final-policy /etc/packaging/final-policy.json \
  --token-file /var/run/packaging-token/token --apply
```

Omit `--apply` for local status. Status does not read the token or policies, make
network requests, or create the database or lock. Apply requires Linux `flock`
and an existing, protected, writable state directory. CronJobs and manual Jobs
must mount the same durable local filesystem and use this same entrypoint.
The lock is held across the invocation; SQLite transactions are not held across
network reads or artifact replay. Use the platform's Job execution deadline to
bound the entire invocation; individual HTTP operations also have byte limits,
socket timeouts and read deadlines.

The existing trusted coordinator policy additionally supplies:

- `service_user_id`: exact positive user ID; `/user` must return that ID and
  `is_admin=false`. The credential comes only from the dedicated token file.
- `builder_image`, `collection`, `architectures`: the single central desired
  product scope. The source is always the freshly read `qos/qos` `main` SHA.
- `final_receipt_policy_sha256`: exact bytes of the separately mounted final
  policy, already required by the product adapter.
- `bundle_compatibility_approved`: explicitly `true` only after the platform
  qualifies the native verifier bundle against this immutable builder and final
  policy. There is no true default and no candidate or dispatch override.

The native bundle descriptor must have approved binary/adapter hashes, size,
source, the same immutable compatible builder, and its fixed-origin digest URL
before dispatch is possible. Both policies must remain unchanged through dispatch
and successful acceptance, including zero-app completion. A changed desired policy
gets a new durable observation sequence; an existing incompatible active run fails
closed rather than silently becoming evidence under the new policy.

The transport uses only `https://code.podesta.ai`: narrowly permitted repository
and identity GETs, and POST only to `release-apps.yml/dispatches`. No proxy or
redirect is followed; no `fgj`, interactive profile or administrator fallback is
used. Downloads share the same bounded transport. Canonical verifier bundle fetch
uses the existing fixed-origin, digest-pinned downloader. The service identity
needs read access to the private repository/artifacts and verifier package, plus
permission to dispatch this workflow. Server-side grants still require separate
qualification; client route restrictions do not replace least-privilege grants.

Every applied invocation observes the latest desired identity and calls the
existing state machine once. Repeated observations do not increment the sequence.
An uncertain dispatch remains `reconcile-required` and is never automatically
resent. This entrypoint does not introduce a reconciliation heuristic or conceal
the outstanding state.

Install these reviewed files outside candidate checkouts in the immutable source
bundle, preserving `tools/` paths:

```
ci_python.py
app_packaging_service.py
app_packaging_product.py
app_packaging_coordinator.py
resolve_ci_builder.py
accept_app_artifacts.py
assemble_verified_apps.py
app_channel_manifest.py
final_app_receipt.py
canonical_verifier_bundle.py
canonical_app_verifier.py
```

The platform must pin that complete source bundle, protect token/policy/state
mounts, supply sufficient scratch disk for bounded artifact extraction, and
qualify the native replay environment before enabling the staged CronJob.
No live API shape, permissions, native replay or continuous packaging acceptance
is claimed by the local controller tests.
