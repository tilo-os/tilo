# Rystic feedback — 2026-10-17

## Dropbox 0.0.4: replacement by ID creates a different file

Reproduced on commit `2cdef26eec206e8d10ec7c750ca9d3aa90f9a07a`.
Affects both `/2/files/upload` and `/2/files/upload_session/finish`.

Given an existing file with ID `I` and revision `R`, upload `changed` using:

```json
{"path":"I","mode":{".tag":"update","update":"R"},"strict_conflict":true}
```

- **Expected:** same ID and path, new revision and contents.
- **Actual:** HTTP 200 with a new ID and the literal old ID as its path.
  Downloading by the original path returns the old contents; downloading by
  the original ID returns the new, misnamed file.

Example:

```text
Original:    id:0000000000000000000001 at /folder/file.txt
Replacement: id:0000000000000000000002 at id:0000000000000000000001
```

[Standalone reproduction](rystic-dropbox-repro.py):

```bash
python3 rystic-dropbox-repro.py
```

Requires Python 3 and Rystic with Dropbox 0.0.4 installed. Creates and cleans up
one instance. Exit 1 reproduces the bug. Uses a dummy bearer token.

## Google Drive 0.0.5: DOCX exports contain placeholder bytes

Last reproduced 2026-10-14; 0.0.5 was still latest on 2026-10-17.

Create an `application/vnd.google-apps.document` file, then request:

```text
GET /drive/v3/files/<id>/export?mimeType=application%2Fvnd.openxmlformats-officedocument.wordprocessingml.document
```

The response is not a ZIP Office Open XML package. Returning a valid DOCX would
allow consumers to test opening exported files.

## Previously reported issues now passing

- **Dropbox 0.0.4, tested Oct 17:** continuation includes newly added files;
  longpoll with a valid cursor returns 200; a 1,048,577-byte upload succeeds.
- **Drive 0.0.5, tested Oct 14:** Changes pagination/cursor resumption and
  resumable uploads pass.
