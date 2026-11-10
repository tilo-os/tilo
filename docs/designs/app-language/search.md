# Search

Every resource is findable, with useful filters and good-looking results, without any search
code. A type makes search better by declaring things about itself. The system does the
searching and draws the UI.

## 1. A query

A query is text, filters, a sort and a scope:

- **Filters** test the resource's own fields: equal to, in a range, contains, is this person.
- **Sort** is a field and a direction, or relevance.
- **Scope** is what the results are inside (a notebook, a thread) or the whole workspace.

The same query value is used everywhere: the system palette, search scoped to one resource, a
list's filter controls, `query(...)` in a view, and an agent's `find`.

## 2. What a type declares

```
type Message {
    subject: Text
    body: Text
    from: MailAddress
    received: Time
    unread: Bool
    labels: Set<Label>
    attachments: Collection<File>

    search {
        filter unread "Unread"
        filter from
        filter received
        filter labels
        filter "Has attachment" = attachments.count > 0
        sort received newest
    }
}
```

- `filter field` picks a control from the field's type: a toggle for `Bool`, chips for enums, date
  presets and a range for times, a range for numbers, a picker for references and identifiers,
  tag chips for `Set<Text>`. The same filter can be typed (`is:unread`, `received:>2026-10-01`)
  or passed by an agent (`unread: true`).
- `filter "Label" = expr` is a named test over the resource's own fields; the compiler checks it
  reads only this resource so it can be indexed.
- A filter is a value of type `Filter<T>`: `Message.unread` names the one the type declares,
  `filters(Message)` lists them, and `items.matching(Message.unread)` asks for it, so a window's
  state can hold the place a person chose without a search string. `Message.labels(label)` is a
  filter with its value (`label:urgent`); a name the type declares no filter for is a compile
  error listing the ones it does.
- `sort` sets the default order. Relevance is offered whenever there's text.
- Text fields: every `Text` and `RichText` field is indexed, and a result shows the record's
  first one, cut to its first line.
- Without a `search` block, every simple field (`Bool`, enum, date, number, reference, set) is a
  filter, and the first time field is the default sort. A Todo with `done` and `due` gets "Done"
  and "Due" filters with no code.
- The first filters are shown as chips; the rest go under "More filters".

## 3. Types, references and paths

Search knows no domain. Mail addresses, phone numbers, contacts and payments are just types,
and three rules cover everything:

1. **One way to write values.** Identifiers are URIs (`mailto:jane@ycombinator.com`, `tel:+49…`,
   `discord:janedoe`), with bare forms where they're unambiguous. `*` is the one pattern
   (`*@ycombinator.com`). Other values have literals (`500EUR`, `5m`, `2026-08`, `-7d`), and `>`,
   `<`, `..` work on all of them. An identifier alone finds it in any field of any type.
2. **Links are ordinary references, and missing ones are missing.** A Discord user with no linked
   Tilo identity shows as that Discord user, nothing more. Who made a link, and how sure it is,
   are fields of the link.
3. **Filters follow references:** `from.contact.org:~"Y Combinator"`. A missing link means no
   match, never a guess, and the answer says so ("matched 4; 9 senders have no contact").

`none` and `any` work on every optional field and set: `invoice:none`, `played:none`.

## 4. What a query names

A query names what something is as often as what it is called: "my mails", "notes", "youtube
videos of <channel>". Every published type has a noun and its plural, and its app has a name, so
search matches those too, with no domain knowledge:

```
type Mailbox (origin: imap.mailbox) {
    name: Text
    role: Text merge register

    search {
        primary role == "inbox"
    }
}

type TeamMember {
    name: Text

    search {
        noun "person" "people"
    }
}
```

- **Nouns.** A type's noun is its name in words (`TodoList`: "todo list"), its plural derived
  ("todo lists", "mailboxes", "entries"). `noun "conversation"` names it otherwise, and a second
  word gives a plural English does not derive. The app's name ("Mail") and its plural ("mails")
  name every type of the app. `meta(r).noun` and `.plural` say the same words.
- **Type words.** A word of the query (or several together, for a noun of several words) that is
  a type's noun or plural, or its app's name or plural, matches every resource of that type
  whatever it is called; the rest of the query must match the resource's text as usual. "mail
  archive" is the Archive mailbox: "mail" names the app, "archive" its title. A word naming
  another type of the same app is about that app's things and need not match a resource's text
  either, but it narrows: "youtube videos" are the client's videos, not its channels. A type
  called as its app is ("Mail" in Mail) names the app.
- **Primary.** `primary <test>` names the one of its type a query naming the type means: a
  test over the resource's own fields, as a named filter is. A root is the app's one resource of
  its type, and stands first when the app declares no primary. When a query matches a type rather
  than a title, the declared primary leads, then the root, then the rest. "my mails" places the
  Inbox; "notes" places the notebook.
- **Filler.** "my", "mine", "our", "the", "all" and "of" are never required to match. What a
  person searches is theirs to read already, so "my" narrows nothing further and is dropped with
  the rest; a title is still compared with the query as typed, so "The Beatles" keeps its exact
  match. A query of filler alone is asked as typed. A sum or a web search is asked word for word.
- **A title beats a type.** A query matching a type ranks its resources above a loose match (a
  typo, words spread over fields) and below any real match of a title: "Groceries" finds the note
  called that before the grocery lists.
- **One path.** A published resource carries what it is (`ResourceSpec::resource_type`), and
  the one search the palette, text typed on the canvas and an agent's `find` all run ranks what
  the session publishes by it (`type_match_score`). No app or type is a special case.

"youtube videos of <channel>" works on this model through the YouTube client, with nothing
built for it. "of" is filler; "youtube" names the client's app; "videos" is the plural of its
`Video` type, another type of the app than `Channel`. So for the client's channel, both words are
type words and the channel's name is all that must match its text: the channel ranks by its
title, with the type bonus, and so does a video whose text names the channel. A video that does
not name it is not found, and "youtube videos" alone lists the videos, not the channels. The
channels are the client's from its origin (§7, "Resources with an outside origin"): its provider
is asked the query without the filler, and what it answers is cached and published like the
rest, so its channels carry their type and are ranked as above. The ranking only adds matches and
never requires a type word to be in a title, so a hit the provider returns keeps its own match.
The client must be named so: a type's `app` is the app's title, and the other words an app is
known by (the registry's keywords: "youtube" for an app called Tube) are not part of it yet (§9).

## 5. Results

- **A type's row view is its result, everywhere:** in lists, the palette, agent answers, embeds.
  There's no separate search row.
- Matches are highlighted automatically (the runtime knows which field each text came from).
  When the match is in a field the row doesn't show, a snippet line is added.
- Without a row view, a generic row shows the title, the first other text, the first time and an
  avatar where there is one.
- The preview shows the type's default view, read-only and live.
- Rows can render without the app running: the system runs the view in a read-only instance
  that sees only the shown resources. Until it loads, the row shows the published entry in the
  same layout, so nothing shifts.
- **Ranking is the system's:** text match and field weight first, then what the person opened
  or wrote recently, then recency. A chosen sort replaces the last two.
- **A saved query is a resource:** a smart folder, a "due this week" list, an agent's standing
  "watch for mail from the school". Synced and shareable like anything else.

## 6. Where search appears

- **The palette:** typed filters work across types; picking a type shows its filter chips with
  counts, and its sort.
- **Scoped to a resource:** a window's search button opens the same search scoped to that
  resource. The list's filters are the type's filters, and typing narrows the list in place.
- **In views:** `list(query(Message, in: box, unread: true, sort: .received_newest))`. The
  arguments are the type's filters, type-checked; the result loads what's shown. A view's query
  finds free text in the type's text fields: a rich text compares as the text it is written as,
  the same for every reader, since what its references read as is not what it holds.
- **Agents:** `find` takes the same query; `describe(type)` lists its filters. Results carry
  their filter values and a match anchor the agent can cite. Agents only see what their task
  may read.

## 7. Who indexes

- **Text search is the system's.** The shell keeps one persistent index of what the workspace
  holds (`tilo_desktop_search::content_index`), published into by whatever holds the resources, and it answers
  even when nothing that published is running. Nobody keeps a private copy.
- **Resources with an outside origin** (an IMAP server, YouTube) are searched through that
  origin's search capability, invoked by the system and merged into one ranking. What was
  fetched is cached and indexed like the rest.
- **The list index** (each list's order and filter columns, so a list opens by reading one
  page) is still kept next to the record, because most workspaces' records aren't yet held by the
  system's storage service. It moves there when they are.
- **A filter's bound value is a run.** A filter may compare a field with a value the code holds
  (`b.cards.filter(c -> c.column == col)`, `col` a local): the value is bound when the filter
  is asked, and the index answers it as the run of that value in the field's column. A view
  showing each value's items and count reads, per run, its bounds and the page it
  starts on; a row saying how many items one run holds counts that run's items, which reads
  its run once and the count from then on, never the workspace; no separate grouping syntax is
  needed.
- **A filter of several tests reads its run by pages.** One test that a column answers as a
  run of one value (an equality, a presence, a named filter, `boxes.contains("inbox")` in a
  set) chooses the run; the other tests in the conjunction (`snoozed_until <= now`,
  `not muted`) are checked item by item as those pages fill. Filling a page reads the run's
  pages until it holds a page's worth of items the others hold of, so the first page costs
  what it examines, not the run's length, and a change that brings one item in (a snooze
  ending, at the moment it ends) reads the item's own pages. Until the whole run is checked
  the list's length is the run's count — the most it can have — and says it is up to that
  many rather than claim it exact. Its `len` is that length, which a column's counts answer;
  its `count` is how many it holds, which reads the run it is drawn from to its end once, so a
  view drawing a run's count says the items it drew for the items it holds, and a workspace's
  list of everything shows the most it can hold rather than read itself.
- **A checked listing is kept across a change.** A listing remembers the run it read and what
  it found of each entry: a change checks the items it wrote again, takes out those that left
  the run, and keeps every other entry's place and membership, so a step costs what it changed
  rather than the workspace. A change the run cannot explain — an entry moved by a write that
  named no item, a read of another run, a run not yet checked whole — drops it, and the listing
  is read again.
- **Versions are indexed too.** Per resource the list index keeps its first and last version
  (who, when, which action: `created(r)`, `updated(r)`) and the last change of each of its
  fields (`changed(r, .field)`), taken change by change from the log, and per collection a
  column of each by time. `items.sort_by(x -> updated(x), desc)` then opens by reading one
  page, and a view showing "edited when, by whom" or grouping by the day a field last changed
  reads no history. The first and last are the earliest and latest by recorded time (then by
  change), so every replica holding the same changes agrees whatever order they arrived in.
- Published entries gain typed facets, so the system can filter, sort and count across
  everything without anything running.

Budgets: first page of a query over 100k resources in 8 ms at p95, facet counts included; one
changed field updates the index in under 0.2 ms; about 200 bytes per resource plus text.

## 8. Acceptance cases

Each must work with only the rules in §3, no domain knowledge:

| Query | Uses |
|---|---|
| `*@ycombinator.com is:unread received:-30d` | identifier pattern, flags, time |
| `Message author.identity.member:false` | an optional link as a path |
| `Event attendees:*@acme.com event.floor:3` | identifiers in a set, a path |
| `Payment amount:>500EUR invoice:none` | a money literal, a missing reference |
| `Contact phone:tel:+49* email:none` | identifier pattern, missing values |

## 9. Open

- Reverse links ("everything from Jane"): which reverse paths a type has indexed.
- Personal ranking signals stay on the person's devices; how they sync between them.
- Search across several workspaces.
- A resource's type carries its app's title alone; the other words its app is known by (the
  registry's keywords) should name its resources too.
- "my" is dropped: published resources carry no owner yet. Once they do, it narrows to the
  person's own.
