# Importing another tool's records

`tilo import` brings what a team already has in another tool into a Tilo
workspace, as that workspace's own records. It reads the source where it
lives, with the address and key you give it, and writes through the workspace's
apps' actions, which a person's click and an agent's call run too. An imported
thing therefore belongs to an app and opens there, is searchable, and keeps who
imported it.

One import is one run: History names it, and undo takes it back in a step per
app. Running an import again follows the source, and a record an earlier import
made is updated where it is.

| Source | Command | Becomes |
| :--- | :--- | :--- |
| Outline | `tilo import outline --url … --token-file … -w …` | Notes: a note per collection, a note per document under it, pictures kept |
| Linear | `tilo import linear --token-file … -w …` | Tasks: issues with body, state, labels, assignee, comments and links |
| Forgejo | `tilo import forgejo --url … --token-file … --repo owner/name -w …` | Tasks: the repository's issues, `#42` as a link to the issue |
| Files | `tilo import files ./folder ./doc.md -w …` | The workspace: folders and files as notes, pictures in Photos, PDFs in PDF Viewer, Markdown as the note's body |
| Mail | `tilo import mail --imap-host … --smtp-host … --user …` | Mail: the account is set up in Mail, which syncs its mail into the workspace |

A key for the source goes in a file given to `--token-file`, or in
`TILO_IMPORT_TOKEN` with `--token-env`. It is never written into the
workspace, into a record, or into the command line.

## What each source keeps

**Outline.** Every collection becomes a top-level note named after it. Every
document becomes a note under its collection, or under the document the
collection nests it in. A document's words are its Markdown, and the pictures
it shows are downloaded and kept with the note: the note shows them even after
the Outline site is gone. A picture larger than a note keeps (1 MiB) stays
where the document links it.

**Linear.** Every issue keeps its identifier, title, description, state,
labels, assignee, comments and the issues its relations name, and its page
address in the task's Source field and at the end of its description. Linear's
states become the tracker's: completed is Done, canceled is Cancelled, started
is In progress, unstarted is To do, and anything else, Backlog. An assignee is
kept as `person:<their name>`; whether that person is a member of the
workspace is the workspace's business, as it is for any assignee.

**Forgejo.** Every issue of the named repositories keeps its number, title,
description, state (open is To do, closed is Done), labels, assignee and
comments. `#42` in an issue's words becomes a link chip to that issue, when
this import brings it.

**Files.** A folder keeps its structure: `contracts/2026/lease.pdf` becomes a
note `lease.pdf` under a note `2026` under a note `contracts`, so the tree
reads as it does on disk. A Markdown or text file is its note's body. A
picture goes into Photos (which keeps one photograph per distinct file, so a
second run adds no second copy) and its note says which record opened it. A
PDF opens in PDF Viewer. A file no app reads still goes into the workspace,
and its note names its `tilo.content:` reference.

**Mail.** Mail's accounts are the person's, not the workspace's: they reach
every device the person signs in on, and Mail syncs each account's messages
into the workspace where agents read them. `tilo import mail` therefore runs on
the computer whose Tilo runs Mail, and adds the account through Mail's own
save-account action. No password is passed on the command line: the account is
added waiting for its sign-in, and Mail asks for the password once, the first
time it opens the account, and keeps it in your vault. Gmail and Outlook block
password sign-in: pass `--auth oauth` and finish that sign-in in Mail.

## What an app needed first

Two apps gained the fields and verbs an import needs, rather than the importer
storing anything itself:

- **Tasks** gained an issue's source page, and an `import-issues` verb that
  keeps an issue's body, state, labels, assignee, thread and links, and updates
  the issue an earlier import made instead of adding another.
- **Notes** gained an `import-pages` verb that keeps a page's words, pictures,
  tags and the page it sits under, and updates a page an earlier import made.

Both are ordinary record actions: any script or agent may run them, and their
effects are in History like any other change's.
