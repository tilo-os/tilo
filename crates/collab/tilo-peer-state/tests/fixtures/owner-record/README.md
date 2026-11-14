# Owner record fixtures

- `single-file.owner.json.hex`: a record as the 0.1 releases (v0.1.5 among
  them) wrote it. Captured on 2026-11-02 from the production writer before the
  record was split into an index and one file per workspace: a real owner
  record as one JSON file, exactly as a device's `state.owner.json` held it.
  It is at revision 3, holding two workspaces this device founded — `Founders`
  (`40267648-f762-4c8d-89ad-c5ab27aa13b8`) and `Research`
  (`7fdaf11b-68aa-4e34-b01d-4ee2424fadcf`), both hosted by its own device — and
  one enrollment in flight into `aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa`, so the
  file carries a workspace, an enrollment in flight and the absent cases beside
  them. Each workspace's entry is version 2 of its layout: the workspace named
  by its UUID as text (`room`) and by a number derived from it (`workspace`).
- `../../../golden/owner-workspace-record/v4.hex`: one workspace's file in the
  layout this build writes (version 4): `Founders` from the record above, as
  this build brings it up and keeps it beside the record.

The identity is a test one: root seed `[11;32]`, device seed `[12;32]`, device
label `A's desktop`; founding and enrollment ran at the fixed time
`testing::raw_fixture_at` returns. The released record's generator was a
temporary ignored test over the real `create_workspace` and
`prepare_enrollment_into` paths; it was removed after capture. Devices hold
that record, so it is never written again: tests decode the hex to bytes and
read them through the production reader, which brings each entry up to this
build's layout, and changing the file means re-checking that an old record
still reads, not re-encoding it.

`examples/regenerate_owner_fixture.rs` writes a version's golden file
(`golden/owner-workspace-record/v<version>.hex`) from the released record. A
version's file is never written again: a new layout is a new version, with its
own golden file and the step that reads the version before it.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `single-file.owner.json.hex` | 16208 | `95783fe0985154a0313471061dc10b3f5178cbc61d9fbbbb590dbd3862ae3560` |
| `golden/owner-workspace-record/v4.hex` | 4097 | `e2050ccfa79b23b90cb3505e9cafaa39f0f0f4b5a56d54ae35413c4583493b20` |

Persisted byte fixtures are stored as `.hex` files: two lowercase hexadecimal
digits per byte, separated by spaces, with 16 bytes per LF line. The decoded
bytes are the original record; byte counts and hashes above refer to decoded
bytes.
