# Files

A workspace keeps files the way it keeps everything else: through the apps, and by reference. Bytes go up
once, the host names them by their hash, and an action takes that name.

## Upload once, name it by its hash

An upload answers a reference that starts `tilo.content:` — for bytes you upload, the hash of those bytes:

```text
tilo.content:30d6761d0551dba00b292649ccd90c63a27057c20f0f3ff24da14e5977442ab5
```

The same bytes uploaded twice are one file, because the name is the hash. A name you pass along with the
bytes is your own label, kept beside the reference rather than sent. One upload takes at most 64 MiB.

## Actions take the reference

The bytes have no screen of their own: an app reads them. So the next step is always an action that names
the reference:

| To | Action |
| :--- | :--- |
| Open a PDF in PDF Viewer | `pdf-viewer#import-pdf-content` with `content` and `name` |
| Read a document in Writer | `word-processor#read-content` with `content` and `name` |
| Put a picture in a note | Notes' `keep-image`, or `place-image` with a page |

And an action that makes a file answers its reference instead of a value: Writer's `export-content`
(`format`: `docx`, `pdf`, `odt`, `md`, `html`, `rtf` and `txt`; PDF is export only), Notes'
`export-markdown-content`, a Browser page's `capture-page`.

## The round trip

Upload a file, make one, and read it back:

```python example=upload-and-export
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Files and letters").id)

# Bytes go up once, and the reference is what the apps take.
reference = workspace.files.upload(b"passport scan, in bytes")
print("uploaded:", reference)

# An action that makes a file answers a reference to it.
workspace.do("word-processor#append-paragraph", text="Dear Visa Officer,")
letter = workspace.do("word-processor#export-content", format="docx")

# And the bytes come back out.
data = workspace.files.download(letter)
print("the letter is", len(data), "bytes, a Word file:", data[:2] == b"PK")
```

```typescript example=upload-and-export
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Files and letters")).id);

// Bytes go up once, and the reference is what the apps take.
const reference = await workspace.files.upload(
  new TextEncoder().encode("passport scan, in bytes"),
);
console.log("uploaded:", reference);

// An action that makes a file answers a reference to it.
await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
const letter = await workspace.do("word-processor#export-content", { format: "docx" });

// And the bytes come back out.
const bytes = await workspace.files.download(String(letter));
console.log("the letter is", bytes.length, "bytes, a Word file:", bytes[0] === 0x50 && bytes[1] === 0x4b);
```

## From a shell

```sh
tilo files put passport.png -w jane-doe                          # prints the reference
tilo -w jane-doe do pdf-viewer#import-pdf-content \
  content=tilo.content:… name=application.pdf
tilo -w jane-doe do word-processor#export-content format=docx     # answers a reference
tilo files get tilo.content:… -w jane-doe -o cover-letter.docx    # or straight to standard output
```

## Who may

A member who may change the workspace's records puts files and takes them out; a viewer only reads. An
agent is held to the same line: its key reaches the one workspace, and its role there decides.

Under the covers an upload is one `PUT` to the workspace's host, which answers the reference; a download
comes back in pieces. Both SDKs and the command do that for you.
