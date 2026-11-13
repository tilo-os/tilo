# Owner record fixtures

Captured on 2026-11-02 from the production writer before the record was split
into an index and one file per workspace: a real owner record as one JSON file,
exactly as a device's `state.owner.json` held it.

- `single-file.owner.json.hex`: one record written as one file, at revision 3,
  holding two workspaces this device founded — `Founders`
  (`40267648-f762-4c8d-89ad-c5ab27aa13b8`) and `Research`
  (`7fdaf11b-68aa-4e34-b01d-4ee2424fadcf`), both hosted by its own device — and
  one enrollment in flight into `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`, so the
  file carries a workspace, an enrollment in flight and the absent cases beside
  them.

The identity is a test one: root seed `[11;32]`, device seed `[12;32]`, device
label `A's desktop`; founding and enrollment ran at the fixed time
`testing::raw_fixture_at` returns. The generator was a temporary ignored test
over the real `create_workspace` and `prepare_enrollment_into` paths; it was
removed after capture. Tests decode the hex to bytes and read them through the
production reader, which splits them once and never again: changing the file
means re-checking that an old record still reads, not re-encoding it.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `single-file.owner.json.hex` | 18122 | `4f30990e5350dde42d7a79fd1bf720326679ec90e77ba2516d51419f727a73ec` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. The decoded
bytes are the original record; byte counts and hashes above refer to decoded
bytes.
