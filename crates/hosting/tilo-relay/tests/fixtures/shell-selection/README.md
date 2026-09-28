# Shell selection fixture

Exported from the real `SessionOwner` publication scenario in
`tilo-device-host/tests/publication.rs`. The identities are created by that test
on every run, so no key outside the repository signs anything here. Regenerate
with:

```sh
rcargo test -p tilo-device-host --test publication --run-ignored only regenerate_shell_selection_fixture
```

`manifest.json` records the authority and certificates, the export time, the
room and routes, and the BLAKE3 hash, length, channel and kind of every signed
Postcard message body. `baseline/` is a full room and application checkpoint;
`tail/` is a room delta plus the touched application snapshot, then a
room-only rename; `checkpoint2/` is the next full checkpoint. The peer exports
and the original signed foreign increment allow exact-state convergence
assertions.

Tests wrap these bodies in synthetic retained transport frames. A zero outer
Envelope signature is not independent authentication: the import models bytes
already admitted through authenticated ingress, with locally pinned shell
authority. The helpers still verify the signed inner messages and exact room
and application receipts.
