# Live private Git channel race

Qualified against actual Forgejo SSH Git on 2026-09-19, with no Actions jobs or
package uploads. Only the new private fixture branches
`probe-atomic-app-channel-20260919` and `probe-atomic-app-artifacts-20260919`
changed. The fixture repository's main branch and all product refs were untouched.

The actual default-dry-run CLI first promoted one manifest with both architecture
receipts using its explicit private apply flag. Tiny immutable Git JSON blobs
carried fixture source/runtime identities; their bytes and SHA256 were checked
before constructing the channel commit. Initial channel commit:
`1e1484d2e8dbef46924115492af9f8d354f24091`.

The race harness prepared two different sibling candidates at that expected old
head. Each used the production-path pilot validator. A thread barrier immediately
before the actual Git pushes ensured both publishers had passed their expected
head checks. Ordinary non-force pushes competed for the same ref:

- `372db30d0cbae9342c883faf5f8182314a2c65c8`: push failed, readback not-current.
- `718131ea799faf032bc923d04239d1933dc457e2`: push succeeded, readback promoted.

The winning manifest advances x86_64 to source
`a335a2752c50eeeadaa128f6b1ef7ecee8ff64d0`; aarch64 retains its exact prior receipt
at `94b3f3bbfd4d061efaf948245e882d3f98e3014f`. No mixed identity is relabeled as
uniform. A separate new CLI process attempted the older source with generation
1000 and was rejected for source ancestry regression. Readback confirmed the
channel remained at the winning commit. No push was retried or rebased.

`evidence.json` records both candidates, complete before/after manifests, actual
push outcomes, readbacks and restarted stale-attempt result. Offline verification:

```sh
python experiments/ci-forgejo/verify-atomic-channel-evidence.py
python -m unittest discover -s tools/tests -p test_atomic_app_channel.py -v
```

The five local tests also exercise actual Git ref races in a temporary bare
server, invalid artifact digest rejection, product push URL denial, and simulated
lost-response readback without retry. Independent review passed.

This qualifies atomic channel-ref behavior for tiny fixture payloads, not real
package availability, package signing/trust, retention or production permissions.
Production channel integration and consumer migration remain deliberately disabled.
