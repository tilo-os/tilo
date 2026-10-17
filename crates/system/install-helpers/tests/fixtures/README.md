# Staging handoff compatibility fixtures

`staging_handoff_oracle.py` preserves the Python implementation solely as a
fixture oracle. It is not installed in the OS image. Regenerate golden JSON
with `generate_staging_handoff_parity.py` and the real native prepared-setup
extractor. Rust integration tests consume the JSON and execute the compiled
handoff and extractor; they do not launch Python.

The native model deliberately rejects a Btrfs identity whose encoded filesystem
UUID differs from its volume. The historical Python oracle accepted that
malformed identity. Tests explicitly assert native refusal and also exercise a
correct UUID-bound Btrfs identity; they do not weaken the native check.

Native migration seed delivery also refuses conflicting existing consent rather
than overwriting it, including when publishing alongside a reclaim record.
