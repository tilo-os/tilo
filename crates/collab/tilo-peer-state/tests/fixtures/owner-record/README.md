# Owner record fixtures

Written on 2026-11-13 by the production writer, as one file, the way a record
that predates the split into an index and one file per workspace sits on disk:
a real owner record exactly as a device's `state.owner.json` held it.

- `single-file.owner.json.hex`: one record written as one file, at revision 3,
  holding two workspaces this device founded — `Founders`
  (`40267648-f762-4c8d-89ad-c5ab27aa13b8`) and `Research`
  (`7fdaf11b-68aa-4e34-b01d-4ee2424fadcf`), both hosted by its own device — and
  one enrollment in flight into `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`, so the
  file carries a workspace, an enrollment in flight and the absent cases beside
  them.

The identity is a test one: root seed `[11;32]`, device seed `[12;32]`, device
label `A's desktop`; founding and enrollment ran at `2025-10-09T08:53:20Z`.
`examples/regenerate_owner_fixture.rs` writes this file again from the real
paths (`create_workspace`, `prepare_enrollment_into` and `raw_fixture_at`) for
when the record's format changes. Tests decode the hex to bytes and read
them through the production reader, which splits them once and never again, so
the file they came from stays readable by the build that wrote it.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `single-file.owner.json.hex` | 18015 | `97769029cec09af1ff8e19d7b9d20e8e8e5ccfa33ecc11e26787338e2f924b4c` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. The decoded
bytes are the original record; byte counts and hashes above refer to decoded
bytes.
