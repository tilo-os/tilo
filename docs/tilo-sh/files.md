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

## Your workspace's folder on your computer

`tilo login` makes the computer one of your devices and gives the workspace a folder in your home, named
after it: `~/Acme` for a workspace called Acme. It is cloud storage with history built in. Files show
up at once and download when opened; copy a tree in and it syncs:

```sh
tilo login tilo1…              # …"Its files are in /home/ada/Acme, kept in sync both ways"
cp -r ~/Documents/contracts ~/Acme/
echo "- call the notary" >> ~/Acme/todo.md
```

A small service (a systemd user unit; `tilo folders stop` stops it, and `tilo status` says how it
stands) keeps the folders mounted: the same filesystem Tilo mounts at `/var/workspace`, synced through
the workspace's host like everything else. On a computer without FUSE the folder is a plain one synced
both ways.

`tilo folders` manages them:

| Command | Does |
| :--- | :--- |
| `tilo folders` | each folder, whether it is in sync, and how much of it is on this computer |
| `tilo folders add "Book club" [path]` | gives another of your workspaces a folder here |
| `tilo folders remove Acme` | removes the folder from this computer; the files stay in the workspace |
| `tilo folders move Acme ~/work/acme` | shows it somewhere else |
| `tilo folders pause Acme` / `resume Acme` | stops syncing for a while; changes made here wait |
| `tilo folders keep ~/Acme/contracts` | keeps files on this computer, to open offline |
| `tilo folders free ~/Acme/videos` | frees their space; they come back when opened |
| `tilo folders auto --joined on` | gives workspaces you join a folder too (your own get one by default) |

Every command takes `--json`. A member who may only read the workspace gets a read-only folder.

## Who wrote it

Every change in the folder is a change of the workspace's files, named for who made it, the same way on
Tilo and on any other computer: a Tilo app or another program by its name, acting for you; otherwise you,
at that device.
Files' History lists the changes of a folder and of each file, and takes any of them back.

Your device vouches for the name: coworkers see which device said so.

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
| Put a folder of files in it, in one change | `files#open` with `files`: a list of `content`, `name` and `dir` (or a `tilo.content:` reference to one), up to 10 000 |
| Open a PDF in PDF Viewer | `pdf-viewer#open_source` with `address` and `name` |
| Open a document in Writer | `word-processor#open_document` with `file`: `content` (a `tilo.content:` reference) and `name` |
| Put a picture in a note | Notes' `keep_image`, or `place_image` with a page |

And an action that makes a file answers its reference instead of a value: Writer's `export_document`
(`format`: `docx`, `odt`, `rtf`, `epub` or `pdf`), its `export_content` for Markdown, `export_text` for
plain text and `export_html` for a page; Notes' `export_note`.

## Folders, moves and the trash

The workspace's folder is on no one computer. So Files' verbs take a path in the workspace
(`Tasks/12 Plan/notes.md`) and no device, and they work whether or not a computer of yours is on:

| To | Action |
| :--- | :--- |
| Make a folder, with the folders on the way to it | `files#new-folder` with `parent` and `name` |
| Move entries into a folder, making it when it is not there | `files#move` with `from`, `entries` and `to` |
| Rename an entry | `files#rename` with `folder`, `entry` and `name` |
| Copy entries, a folder with all it holds | `files#copy-into` with `from`, `entries` and `to` |
| Put entries in the trash | `files#trash` with `folder` and `entries` |
| Bring them back to where they were | `files#restore` with `entries` |
| See what a folder holds | `files#list` with `folder`; `trash: true` for what the trash holds |
| Read what a file says | `files#read-text` with `file`, its path |
| Read what a file a message carries says | `files#read-text` with `content`, its `tilo.content:` address |
| Write a file | `files#write-file` with `folder`, `name`, and its `text` or its `content` |

A path starts at the workspace's folder, with no leading `/` and no `..`, and `""` is that folder
itself. No verb writes over a file: a name that is taken is refused, and a copy beside what it
copies becomes `plan (2).md`. Each answers the entries it touched with where they were and where
they are, and as `undo` the one call that takes it back. Naming a `device` and absolute paths
instead changes that computer's own disk, through Files on it.

`list` answers the folder's entries by name, each with its `name`, its `path`, its `kind` (`folder`
or `file`), a file's `size` in bytes and `changed`, the time it last changed in milliseconds since
1970. It answers 200 at a time: `next` is the `from` of the page after it, `limit` asks for fewer,
and `total` is how many the folder holds. With `trash: true` it answers what the trash holds, each
with the `id` that `files#restore` brings it back by and where it `was`.

`read-text` answers a file's `text`, 65 536 characters at a time, with `next` and `total` the same
way. It reads text up to one mebibyte: a document, a spreadsheet, a PDF or a picture is refused
with the name of what reads it. Both read where the workspace is kept, so neither opens a window
nor needs a computer of yours to be on.

A bare name (`notes.md`, with no folder before it) that an agent asks on a computer where Files
is open is first the entry by that name of the folder Files shows there, as the agent sees it
listed. A name that folder does not hold, every path with a folder in it, and every name asked
where Files is not open (an agent working on your server) is a file of the workspace's folder.

`write-file` writes a file into a folder, making the folders on the way: from `text`, up to 256
KiB, or from `content`, the `tilo.content:` reference an upload or another app's export answered,
so a PDF Writer made lands in a task's folder. A name that is taken is refused; `replace: true`
gives that file the new content. Like the verbs it answers as `undo` the one call that takes it
back: the trash for a new file, and for one written over the write of what it held before.

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
workspace.do("word-processor#append_paragraph", text="Dear Visa Officer,")
letter = workspace.do("word-processor#export_document", format="docx")

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
await workspace.do("word-processor#append_paragraph", { text: "Dear Visa Officer," });
const letter = await workspace.do("word-processor#export_document", { format: "docx" });

// And the bytes come back out.
const bytes = await workspace.files.download(String(letter));
console.log("the letter is", bytes.length, "bytes, a Word file:", bytes[0] === 0x50 && bytes[1] === 0x4b);
```

## From a shell

`tilo files put` uploads the bytes and writes each file into the workspace's folder. Each line it prints
is the file's path there:

```sh
tilo files put passport.png -w jane-doe                          # passport.png
tilo files put scan.pdf -w jane-doe --to forms                   # forms/scan.pdf
tilo files put ./contracts -w jane-doe                           # a folder goes up whole, keeping its structure
tilo files put config.json -w jane-doe --store                   # the bytes only: prints tilo.content:…
tilo -w jane-doe do pdf-viewer#open_source \
  address=tilo.content:… name=application.pdf                    # an app that takes a reference, by hand
tilo -w jane-doe do word-processor#export_document format=docx   # answers a reference
tilo files get tilo.content:… -w jane-doe -o cover-letter.docx   # or straight to standard output
```

`--store` writes no file: the bytes are in the workspace, named by their hash, for an action you name
yourself. Folders keep their names from the folder you gave: `tilo files put notes/` uploads
`notes/2026/plan.md` as `notes/2026/plan.md`, and `--to docs` puts it at `docs/notes/2026/plan.md`.

## Who may

A member who may change the workspace's records puts files and takes them out; a viewer only reads. An
agent is held to the same line: its key reaches the one workspace, and its role there decides.

Under the covers a put is `PUT /v1/workspaces/<workspace>/contents/<hash>`, the lowercase BLAKE3
hash of the bytes, one request or, for a large file, pieces with `Content-Range`; a download is
`GET` of the same route, with `Range`. Both SDKs and the command do that for you.
