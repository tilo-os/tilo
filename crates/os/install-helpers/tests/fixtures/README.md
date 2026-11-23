# Staging handoff compatibility fixtures

`staging_handoff_parity.json` and `staging_handoff_authenticated_parity.json`
are golden cases first recorded from the Python helper the native handoff
replaced. Rust integration tests consume the JSON and execute the compiled
handoff and extractor. Edit a case only together with the behaviour it pins.

The native model deliberately rejects a Btrfs identity whose encoded filesystem
UUID differs from its volume. The historical Python helper accepted that
malformed identity. Tests explicitly assert native refusal and also exercise a
correct UUID-bound Btrfs identity; they do not weaken the native check.

Native migration seed delivery also refuses conflicting existing consent rather
than overwriting it, including when publishing alongside a reclaim record.
