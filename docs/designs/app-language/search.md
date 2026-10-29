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
    subject: Text (title)
    body: Text (body)
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
- `sort` sets the default order. Relevance is offered whenever there's text.
- Text fields: `(title)` weighs most, `(body)` is prose, others are metadata, `(secret)` is never
  indexed.
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

## 4. Results

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

## 5. Where search appears

- **The palette:** typed filters work across types; picking a type shows its filter chips with
  counts, and its sort.
- **Scoped to a resource:** a window's search button opens the same search scoped to that
  resource. The list's filters are the type's filters, and typing narrows the list in place.
- **In views:** `list(query(Message, in: box, unread: true, sort: .received_newest))`. The
  arguments are the type's filters, type-checked; the result loads what's shown.
- **Agents:** `find` takes the same query; `describe(type)` lists its filters. Results carry
  their filter values and a match anchor the agent can cite. Agents only see what their task
  may read.

## 6. Who indexes

- **Text search is the system's.** The shell keeps one persistent index of what the workspace
  holds (`tilo_desktop_search::content_index`), published into by whatever holds the resources, and it answers
  even when nothing that published is running. Nobody keeps a private copy.
- **Resources with an outside origin** (an IMAP server, YouTube) are searched through that
  origin's search capability, invoked by the system and merged into one ranking. What was
  fetched is cached and indexed like the rest.
- **The list index** (each list's order and filter columns, so a list opens by reading one
  page) is still kept next to the record, because most workspaces' records aren't yet held by the
  system's storage service. It moves there when they are.
- **Lanes are runs.** A filter may compare a field with a value the code holds
  (`b.cards.filter(c -> c.column == col)`, `col` a local): the value is bound when the filter
  is asked, and the index answers it as the run of that value in the field's column. A board
  showing each column's cards and count reads, per lane, the run's bounds and the page it
  starts on, never every card; no separate grouping syntax is needed.
- **Versions are indexed too.** Per resource the list index keeps its first and last version
  (who, when, which action: `created(r)`, `updated(r)`), taken change by change from the log,
  and per collection a column of each by time. `items.sort_by(x -> updated(x), desc)` then
  opens by reading one page, and a view showing "edited when, by whom" reads no history. The
  first and last are the earliest and latest by recorded time (then by change), so every
  replica holding the same changes agrees whatever order they arrived in.
- Published entries gain typed facets, so the system can filter, sort and count across
  everything without anything running.

Budgets: first page of a query over 100k resources in 8 ms at p95, facet counts included; one
changed field updates the index in under 0.2 ms; about 200 bytes per resource plus text.

## 7. Acceptance cases

Each must work with only the rules in §3, no domain knowledge:

| Query | Uses |
|---|---|
| `*@ycombinator.com is:unread received:-30d` | identifier pattern, flags, time |
| `Message author.identity.member:false` | an optional link as a path |
| `Event attendees:*@acme.com room.floor:3` | identifiers in a set, a path |
| `Payment amount:>500EUR invoice:none` | a money literal, a missing reference |
| `Contact phone:tel:+49* email:none` | identifier pattern, missing values |

## 8. Open

- Reverse links ("everything from Jane"): which reverse paths a type has indexed.
- Personal ranking signals stay on the person's devices; how they sync between them.
- Search across several workspaces.
