# Search: finding resources, by default

Every tilolang resource is findable with good-looking, fast results and useful filters, with no
code. An app makes search better by **declaring** things about its types, never by writing search
code or drawing a search UI.

One query model serves:
- the system palette;
- the app's header search;
- list filters;
- tilolang code;
- agents.

## 1. Where we are

| Part | Today | Limit |
|---|---|---|
| Platform search (`ui-core/src/search`) | providers, `SearchQuery` (text, `key:value` facets, scope), ranker with field weights, palette with kind chips | facets are text equality only; no ranges, people or sort |
| Result rows | `SearchRow`: `Message`, `Media`, `FileHit`, or a generic row | closed set; an app can't show its own row |
| System index (`IndexEntry`) | title, subtitle, group, icon, weighted short fields | flat text; can't filter by date, person or state without the app |
| tilolang (`tilolang-tilo/src/search.rs`) | every `Text` field of every record and item, incremental, off the frame | no weights beyond the first text, no filters, generic rows |
| tilolang list filters | an enum in window state becomes a segmented control | a second filter model beside search's |
| Agents (`tilo_find`) | text query over the same index | no typed filters or sort; results are titles |

## 2. The model

### 2.1 A query

```
Query<T> = text + filters + sort + scope
```

- **Filters** are predicates over the resource's own fields: equality, a range, membership, a
  person.
- **Sort** is a field and a direction, or relevance.
- **Scope** is a resource the results are inside (a notebook, a thread) or the workspace.

A `Query<T>` is an ordinary tilolang value and a lazy `Collection<T>`. Wherever a query appears,
the same index answers it:
- the palette;
- a list's filter controls;
- `list(query(Message, unread: true))` in a view;
- an agent's `find`.

### 2.2 What a type declares

A type may have a `search` block. Order is priority: the first filters are the visible chips, the
rest go under "More filters".

```
type Message {
    subject: Text (title)
    body: Text (body)
    from: Person
    to: Set<Person>
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
        hide to
    }
}
```

- **`filter field`** derives the control from the field's type. The label is the field's name or
  its doc comment's first words, and its icon comes from the type or variant.

  | Field type | Control | Typed token | Agent argument |
  |---|---|---|---|
  | `Bool` | toggle chip | `is:unread` | `unread: true` |
  | enum | chips with the variants' icons; several may be picked | `status:open` | `status: [.open]` |
  | `Time`, `Date` | presets (today, this week, last 30 days, a month) plus a range picker; also a sort | `received:>2026-10-01` | `received: 2026-10-01..` |
  | `Int`, `Number` | range | `size:>10` | `size: 10..` |
  | `Person`, `Set<Person>` | people picker with avatars, ordered by who you deal with most | `from:jane` | `from: person:a1` |
  | a reference or `Set` of another type | picker of that type, using its row view | `label:legal` | `labels: [label:9c]` |
  | `Set<Text>` | tag chips with counts | `tag:urgent` | `tags: ["urgent"]` |

- **`filter "Label" = expr`** is a named predicate over the resource's own fields. The compiler
  checks that it is pure and reads only this resource, so the index can keep it incrementally with
  the runtime's field-precise invalidation. It is shown as a toggle chip.
- **`sort field newest|oldest|a_z|z_a`** is the default order when there's no text, and an
  offered order otherwise. Relevance is always offered when there is text.
- **`hide field`** keeps a field out of filters. It is still searchable by text if it is text.
- **Text fields:**
  - `(title)` is weighted as a title and `(body)` as long prose.
  - Other text fields are metadata.
  - `(secret)` fields are never indexed.

**Without a `search` block, the defaults are:**
- every top-level `Bool`, enum, `Date`/`Time`, `Person` and `Set` field is a filter, in field
  order;
- the first `Time` field, or the platform's modified time when there is none, is the default sort,
  newest first;
- text is indexed as above.

So a Todo with `done: Bool` and `due: Option<Date>` gets "Done" and "Due" filters and a "Due"
sort with no code.

**Filters replace the enum-in-window-state rule.** Todo's `enum Show { all, open, done }` and
its `window show` go away. The list's filter control *is* the `done` filter. One model:
- window state still holds the person's current query;
- it is the same query value the palette and agents use.

### 2.3 Results are row views

A type's `row view` is its result view **everywhere**:
- in-app lists;
- the system palette;
- agent answers;
- another app's embed.

There is no separate search row.

- **Highlighting is automatic.** The runtime knows which field each text node of a view came from
  (field-precise reads), so the platform highlights the matched spans in whatever the row shows.
- **Snippets are automatic.** When the match is in a field the row doesn't show (usually the
  body), the platform adds one snippet line under the row from the body search's location.
- **Without a row view,** the platform's generic row shows:
  - the title;
  - the first other text;
  - the first `Time` as meta;
  - the first `Person` as the avatar;
  - the type's icon.
- **Preview.** The palette's preview pane shows the type's `default view`, read-only and live.
- **Grouping.** Results are grouped by type ("Messages", "Notes"); the plural comes from the type
  name or a declared `plural`.

**Rows render without the app running.** Views are pure wasm over resources, so the search host
may instantiate a package's module in a **view-only instance**:
- it may read only the resources whose rows it shows, and nothing else;
- it has no actions; activating a row opens the app;
- it uses one engine for every package, with modules cached;
- it renders only visible rows: at most the palette's 8, or a list viewport.

Until a module is loaded, the row is the generic row from the published entry, in the same row
layout, so nothing shifts.

### 2.4 Ranking

The platform ranks; an app writes no ranking code:
1. Text match tier and field weight (existing `rank.rs`).
2. Then the person's own signal: what they opened recently and what they wrote.
3. Then the default sort field's recency.

When the person picks a sort, it replaces 2 and 3 but not text matching. A text query never shows
non-matching results.

### 2.5 Saved searches

A `Query<T>` saved is a resource (`std.SavedSearch<T>`): a smart folder. Mail's "From YC, unread"
in its sidebar, a Todo list's "Due this week" and an agent's standing "watch for mail from the
school" are all saved searches. They are synced, collaborative and shareable like any resource.

## 3. Surfaces

### 3.1 The system palette

- **Typed tokens.** With no type chip selected, typed tokens for any declared filter key work
  across types: `from:jane` matches every type with a `Person` filter called `from`.
- **Type chips.** Selecting one type chip (Messages) shows that type's filter strip, with counts:
  `[Unread 12] [From ▾] [Received ▾] [Labels ▾] [More]`. Picking a chip and typing a token are the
  same filter, shown as the same chip in the field.
- **Sort.** The sort menu appears when there's a type chip.

### 3.2 In the app

The platform header's search (`native-apps.md` §3) runs the same query, scoped to the app's
root:
- the list's filter strip below the header is the type's filters;
- the list *is* the results;
- there is no separate "search results" mode: typing narrows the list in place, with matches
  highlighted.

On narrow widths the strip collapses to one "Filters" button showing the active count.

### 3.3 In tilolang code

```
view inbox(box: Mailbox) {
    list(query(Message, in: box, unread: true, sort: .received_newest))
}

view due_soon(l: TodoList) {
    section("This week") {
        list(query(Todo, in: l, done: false, due: today()..today() + 7d))
    }
}
```

- The arguments of `query` are the type's filters, type-checked.
- The result is a lazy collection answered by the index. Opening costs what is shown.

### 3.4 Agents

`find` takes the same query, and `describe(type)` returns the type's filters with their
descriptions:

```
describe(mail.Message)
→ filters: unread (Bool) "Unread", from (Person) "Who sent it", received (Time),
           labels (Set<Label>), has_attachment (Bool) "Has attachment"
  sort: received newest | relevance

find(mail.Message, text: "83(b)", where: { from: org:8f2.people, received: 2026-09-01.. },
     sort: received newest, limit: 10)
→ [ { ref: msg:m3, title: "Re: Incorporation & SAFE", from: person:a1, received: 2026-10-21T14:02,
      unread: true, labels: [label:9c "Legal"], match: { field: body, at: ¶2.2,
      excerpt: "File your 83(b) election within 30 days…" } }, … ]
```

- Results carry the filter fields' values, so an agent can reason without reading every
  resource.
- `match` is a content anchor the agent can cite (`quote(msg:m3, ¶2.2)`).
- Agents search under their task's authority. They see only what the task may read.

## 4. Index and performance

- **Per package, per workspace, persistent.**
  - The index is kept incrementally from the store's field changes, off the frame, as
    publication is today.
  - It is stored so that opening never rebuilds it.
  - It is updated per changed field, never per resource or per room.
- **Typed.** It holds text postings plus typed columns per filter: booleans, enums, times,
  numbers, references and set members. Equality, ranges and membership are answered from the
  columns. Facet counts come from the same columns.
- **Published to the system index.**
  - `IndexEntry` gains typed facets: key and value, where the value is text, number, time, bool,
    enum or ref.
  - The package publishes its types' filter schema once: keys, labels, kinds, icons, variants.
  - The system index can therefore filter, sort and count across apps without any app running.
- **Budgets,** checked by counters in tests and by wall clock in the bench:

  | Measure | Budget |
  |---|---|
  | First page of a query, palette or in-app, 100k resources of a type, laptop, p95 | 8 ms |
  | Facet counts for the visible chips | included in the 8 ms |
  | Index update for one changed field | O(changed field); under 0.2 ms |
  | Index memory | ≤ 200 bytes per resource plus text postings |
  | Rows rendered by view-only instances | only those visible; first palette row ≤ 1 frame after the module loads |

## 5. What changes

1. **Language:**
   - the `search` block, its defaults and checks;
   - `query(T, …)` as a lazy collection;
   - the enum window-state filter rule removed;
   - Todo, Notes, Chat and Board migrated.
2. **tilolang-tilo:**
   - the typed, persistent, incremental index per package;
   - query and facet answers;
   - the same index answers lazy collections' filters.
3. **ui-core search:**
   - typed facets in `SearchQuery` and `IndexEntry`;
   - range, date, people and reference pickers in the filter strip;
   - comparison tokens;
   - sort;
   - automatic highlight and snippet;
   - rows as row views (`components.md`), retiring the closed `SearchRow` for tilolang types.
4. **Search host:** view-only instances of packages for rows and previews.
5. **Agent door:** `find` with typed `where` and `sort`; `describe` with filters; results with
   facet values and match anchors.
6. **A reference app: Mail.** A local Mail package with threads, people, labels, attachments and
   unread state. It exists to prove the defaults on the richest common case, and it is the target
   of the agent example ("what does YC legal say are our next steps?").

## 6. Open

- **Cross-type filters by shared meaning.** "From Jane" across messages, notes she wrote and
  her todos assigned to you. Field names alone (`from`) are fragile. `Person` filters may need a
  declared role (author, assignee, participant).
- **Ranking learned from the person** must stay private to their devices. How it syncs between
  their own devices is open.
- **Search across workspaces:** the palette's "Everywhere" over several workspaces' indexes.
