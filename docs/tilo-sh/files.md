# Files

A workspace has one folder. It is the workspace's file tree, every member's Files window shows it, and a
machine that logged in sees it as `~/<workspace>`. A file gets into a workspace by being written there.

Bytes go up once, the host names them by their hash, and a file in the tree names that hash.

```sh
tilo files put passport.png -w jane-doe             # passport.png
tilo files put ./contracts -w jane-doe --to docs   # docs/contracts/…, the folder kept
```

The upload puts the bytes in the workspace and writes the file into the tree, at the folder `--to` names,
under the names they have here. The same name put again takes the new content, as a copy over a file does.

## Upload once, name it by its hash

An upload answers a reference that starts `tilo.content:` — for bytes you upload, the hash of those bytes:

```text
tilo.content:30d6761d0551dba00b292649ccd90c63a27057c20f0f3ff24da14e5977442ab5
```

The same bytes uploaded twice are one file, because the name is the hash. An action that puts the file
somewhere takes the name it goes by as an argument of its own. One request carries at most 64 MiB;
a larger file goes up in pieces, which `tilo files put` does for you.

## Actions take the reference

The bytes have no screen of their own: an app reads them. So the next step is always an action that names
the reference:

| To | Action |
| :--- | :--- |
| Put a file in the workspace's folder | `files#open` with `content`, `name`, and `dir` for a folder |
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

```curl example=upload-and-export
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Files and letters"}' | jq -r .id)
base=$api/workspaces/$workspace

# Bytes go up once, and the reference is what the apps take.
reference=$(printf 'passport scan, in bytes' | curl -sS --fail-with-body "$base/uploads" -H "$auth" \
  -H "Content-Type: application/octet-stream" --data-binary @- | jq -r .content)
echo "uploaded: $reference"

# An action that makes a file answers a reference to it.
curl -sS --fail-with-body "$base/resources/word-processor/actions/append-paragraph" -H "$auth" \
  -H "Content-Type: application/json" -d '{"text": "Dear Visa Officer,"}' > /dev/null
letter=$(curl -sS --fail-with-body "$base/resources/word-processor/actions/export-content" -H "$auth" \
  -H "Content-Type: application/json" -d '{"format": "docx"}' \
  | jq -r '[.. | strings | select(startswith("tilo.content:"))][0]')

# And the bytes come back out.
curl -sS --fail-with-body "$base/content/$letter" -H "$auth" -o letter.docx
echo "the letter is $(wc -c < letter.docx) bytes, a Word file: $([[ $(head -c 2 letter.docx) == PK ]] && echo yes)"
```

## From a shell

`tilo files put` uploads the bytes and writes each file into the workspace's folder. Each line it prints
is the file's path there:

```sh
tilo files put passport.png -w jane-doe                          # passport.png
tilo files put scan.pdf -w jane-doe --to forms                   # forms/scan.pdf
tilo files put ./contracts -w jane-doe                           # a folder goes up whole, keeping its structure
tilo files put config.json -w jane-doe --store                   # the bytes only: prints tilo.content:…
tilo -w jane-doe do pdf-viewer#import-pdf-content \
  content=tilo.content:… name=application.pdf                    # an app that takes a reference, by hand
tilo -w jane-doe do word-processor#export-content format=docx    # answers a reference
tilo files get tilo.content:… -w jane-doe -o cover-letter.docx   # or straight to standard output
```

`--store` writes no file: the bytes are in the workspace, named by their hash, for an action you name
yourself. Folders keep their names from the folder you gave: `tilo files put notes/` uploads
`notes/2026/plan.md` as `notes/2026/plan.md`, and `--to docs` puts it at `docs/notes/2026/plan.md`.

## Who may

A member who may change the workspace's records puts files and takes them out; a viewer only reads. An
agent is held to the same line: its key reaches the one workspace, and its role there decides.

Under the covers an upload is one `POST /v1/workspaces/<workspace>/uploads`, which answers the reference;
a download is `GET /v1/workspaces/<workspace>/content/<reference>`. Both SDKs and the command do that for
you.
