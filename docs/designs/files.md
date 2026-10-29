# Files and apps

Status: draft, still being thought through.

## Where meaning lives

- A **file** is data whose format is the contract: its meaning is in the format, so any program
  can open it.
- An app **resource** is data whose model is the contract: its meaning is in types and actions
  (merge rules per field, permissions per action, references between things).

So the line between them is where the meaning lives, not old versus new:

- **Files:** things whose value is that many programs can use them. Photos, audio, video, PDFs,
  code, datasets, documents exchanged with the outside world.
- **Resources:** things whose value is structure and behaviour. A todo list, a conversation, a
  board, contacts, a calendar, comments. Nobody wants `chat.json`; they want the conversation.
- **Both, with one truth:** a document people exchange (`.docx`) has the file as the truth and
  the app as a structured editor over it with CRDT merges. A note born in Notes has the resource
  as the truth and files as exports. Never two copies synced both ways independently.

## Files in Tilo

The workspace's file tree is synced between devices, versioned, and every move and edit is an
action like any other. That removes what made files second-class (no history, no sync, no
collaboration, lost edits), so files are a good home for format-shaped data, not a legacy.

- **`~/` is the workspace's file tree:** in the shell (ush), in tilolang (`files`) and
  everywhere else. A person's home is the workspace, not one machine's disk.
- **Apps keep format-shaped content as files, and their own structure as resources about
  those files.** Photos: the images are files; albums, faces, ratings and edits are resources
  referencing them. An app indexes its kind of file anywhere in `~/`, not only in a folder of
  its own, so nothing is lost by moving it.
- **Apps don't scatter.** No dotfiles or app-private directories in the tree. App settings and
  app-native data are workspace resources, not files; the tree's structure belongs to the
  person.
- **App-native resources don't appear in `~/` as generated files by default.** They're reached
  through apps, tilolang and the door. A projection for Unix tools can be asked for; by default it
  would make every app's internal format a public promise.

## Metadata travels with the file where the format allows

Facts about a file (a photo's caption, rating, people, place) are written into the file where its
format has a place for them (EXIF, XMP, ID3, document properties), so they survive leaving Tilo.
Where a format has no place for something, it stays a resource, and completeness isn't promised.

## Conversions are the system's

Turning a file into another form ("a smaller copy as JPEG", "this page as PDF", "audio only") is
a system capability backed by the format codecs, instant and the same in every app, not something
each app implements.

## In tilolang

`File` and `Folder` are ordinary resource types. Moving, copying and renaming are actions with
undo and attribution. Codecs give typed views of a file (`f.as(Document)`), conversions are
actions, and `grep` covers files and resources alike.

## Open

- Which apps keep resource-shaped data (settings, state) as files today, and move it into
  resources.
- How far the ush shell's history and undo are the same as the workspace's actions.
- Whether folders are also collections of references (a thing in several places at once), or
  stay a strict tree for files.
