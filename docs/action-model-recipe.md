# Building an app on resources, actions and views

Every Tilo app is written one way. Its state is a typed **model** stored in a
CRDT document, every change is an **action** on that model, and its pixels
are **views** that read the model and submit actions. People, collaborators,
agents, Shortcuts, the palette, search and accessibility all reach the app
through the same published actions, so a button and an agent can never do
different things. The contract behind this recipe is
[docs/architecture/resources.md](architecture/resources.md); permissions are
in [docs/architecture/permissions.md](architecture/permissions.md).

This recipe builds a small bookmarks app. `apps/snippets` is the full-size
reference, and `examples/apps/` has one small app per idea.

## 1. The model

The state is a struct with `#[derive(Model)]`. The derive is the whole
storage contract; an app never touches the document.

```rust
use ui::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Bookmark::valid)]
pub struct Bookmark {
    /// Names the bookmark in history, search and agents' views.
    #[model(label)]
    pub title: String,
    pub url: String,
    pub favorite: bool,
    pub tags: Set<String>,
    /// Changed only by the `visit` verb.
    #[model(readonly)]
    pub visits: Counter,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Bookmarks {
    pub bookmarks: Collection<Bookmark>,
}

impl Bookmark {
    /// True after every change, from any path: a field edit, a verb, an
    /// agent's edit.
    pub fn valid(&self) -> Result<(), Refusal> {
        if self.url.trim().is_empty() {
            return Err(Refusal::new("a bookmark needs an address"));
        }
        Ok(())
    }
}
```

Pick each field's type for how concurrent edits should merge: a `String`
merges character by character, a `bool` or number is last-writer-wins, a
`Counter` adds up, a `Set` merges per element, a `Collection` keeps a stable
id per item so concurrent adds keep both. Short codes that should not merge
characters are `Atom`s.

## 2. Paths and field edits

The derive gives every field a typed path, and every path offers the edits
its type supports. Editing a field is a built-in action: there is no
`set_title` verb to write.

```rust
let p = Bookmarks::paths().bookmarks.item(&id);
let star = p.favorite.set(true);             // Edit<Bookmarks>
let retitle = p.title.replace("Tilo docs");  // the smallest splice
let tag = p.tags.insert(&"docs".to_owned());
```

An edit replays, syncs, undoes and is named in history like any action, and
applies at its path without reading the rest of the record. The model's
`check` runs after it. `#[model(readonly)]` fields offer no edits.

## 3. Verbs

Operations that are more than setting a field are methods:

```rust
#[resource(kind = "bookmarks.library", label = "Bookmarks")]
impl Bookmarks {
    /// Add a bookmark.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, url: String, title: String) {
        self.bookmarks.push(cx.id(), Bookmark { url, title, ..Bookmark::default() });
    }

    /// Delete a bookmark.
    #[action(destructive)]
    pub fn delete(&mut self, bookmark: &Id) -> Result<(), Refusal> {
        self.bookmarks.remove(bookmark).map(drop).ok_or_else(|| Refusal::new("no such bookmark"))
    }

    /// Record a visit.
    #[action]
    pub fn visit(&mut self, bookmark: &Id) {
        if let Some(b) = self.bookmarks.get_mut(bookmark) { b.visits.add(1) }
    }
}
```

- A verb's id is its method name in kebab case (`add`, `delete`); its doc
  comment is its label. `Bookmarks::ADD` is its typed reference, and
  `bookmarks::Add { url, title }` its parameter struct (a module named after
  the record type).
- A verb whose first parameter is `&Id` is also offered on each item, with
  the item bound.
- `destructive` asks for confirmation from people; returning `Err` refuses
  the verb and changes nothing.
- A verb runs again on every replica, so its body must not read a clock,
  randomness, files, the network, the environment or another process. Mint
  ids with `cx.id()`, read the time with `cx.now_ms()`, and take everything
  else as a parameter. `tilo-app check` refuses such reads.

## 4. The view

A view reads the model and binds controls to paths and verbs:

```rust
pub struct App;

impl ResourceApp for App {
    type Resource = Bookmarks;
    const ID: &'static str = "org.example.bookmarks";
    const TITLE: &'static str = "Bookmarks";

    fn view(cx: &mut ViewCx<Bookmarks>) -> Element {
        // A form: the new bookmark's parameters, local to this window,
        // never synced or undone, submitted as one invocation of `add`.
        let form = cx.form::<bookmarks::Add>();
        let url = cx.input(form.paths().url).with_placeholder("bookmarks-url-placeholder");
        let add = cx.button("bookmarks-add", form.submit());

        let rows = cx.each(|library| &library.bookmarks, |row| {
            let Some(id) = row.item().cloned() else {
                return Box::new(Column::new()) as Element;
            };
            let p = row.paths();
            Box::new(ui::row![
                row.checkbox("bookmarks-favorite", p.favorite),
                row.input(p.title),
                row.button("bookmarks-delete", Bookmarks::DELETE.with(id)),
            ])
        });
        Box::new(ui::column![ui::row![url, add], rows])
    }
}
```

- `cx.paths()` are the scope's paths: the record's at the root, the item's in
  a row of `cx.each`.
- `cx.input(path)`, `cx.checkbox(label, path)`, `cx.control(make, path)` show
  the value at the path and submit field edits; typing a word or dragging a
  slider is one undo step. `cx.control_map(make, path, show, store)` binds a
  control whose value is another type (a slider for an `Option<f64>`).
- `cx.button(label, …)` submits a verb (`Bookmarks::DELETE.with(id)`), a field
  edit (`p.favorite.set(true)`) or a form (`form.submit()`).
- `cx.text(|state| …)` and `cx.value(|state| …)` read anything with a closure;
  `cx.value_at(&path)` reads one field.
- A control bound to a path wakes only when a change touched that path; one
  bound with a closure re-reads after every change. Prefer paths in large
  lists.
- Labels and placeholders are Fluent message ids; `tilo-app check` fails on
  one that `locales/en-US.ftl` lacks.
- A control bound to an action the resource does not publish, or to a
  read-only field, fails under `TestApp` and `tilo-app check` rather than
  greying out.
- `cx.on_open_item(|id| …)` shows the item a search hit or an agent's `open`
  names, and may refuse one that is gone.

Verbs taking a value from a control use the closure forms:
`cx.input_with(|state| state.query.clone(), Library::SEARCH)` invokes the
verb with each committed text.

## 5. Hand-written surfaces

A custom widget (an editor, a canvas) takes part through `ResourceSurface`.
It submits the same typed values through the dispatcher it is given in
`attach`, never an action id or an argument map:

```rust
fn attach(&mut self, cx: SurfaceCx<Bookmarks>) {
    self.dispatch = Some(cx.dispatcher());
}

// After an event:
dispatch.submit(Bookmarks::paths().bookmarks.item(&id).favorite.set(true));
dispatch.submit(Bookmarks::VISIT.with(id.clone()));
// Inside the event, when the widget must see its own edit at once:
dispatch.submit_now(&mut ctx.edits(), path.splice(at, 0, "x"))?;
```

`TextControl::at(control, dispatcher, path)` binds a `TextInput` or
`CodeEditor` to a text field: each keystroke is a splice at the path.

## 6. History messages

History renders each change in the reader's language from the app's
catalogue, by convention: `<app>-<type>-<field>` for a field edit,
`<app>-<verb>` for a verb, where `<app>` is the last part of the app id:

```ftl
bookmarks-bookmark-favorite = { $new ->
    [true] Starred { $item }
   *[false] Unstarred { $item } }
bookmarks-bookmark-title = Retitled { $item }
bookmarks-add = Added { $title }
```

Without a message the framework says it generically from the field's doc
comment. `tilo-app check` lists the missing ones.

## 7. The public surface

`tilo-app surface` writes the app's verbs, their parameters and its stored
fields to `surface.toml` (ids, kinds, footprints, paths and types; labels and
descriptions stay in the code); commit it. `tilo-app check` and CI fail when the
code and the file disagree, when a stored field is read differently without
a new `#[resource(version = N, migrate = …)]`, or when a control is bound to
something the resource does not publish. The packaged file is what a
receiver without the app decides its changes against.

A document that does not read as the model shows why in place of the view,
and every action reports itself unavailable for that reason; the last state
is never shown as current.

## 8. Testing

```rust
// No window: the verb and the model's check.
let mut library = Replica::<Bookmarks>::new();
library.apply(Bookmarks::ADD.with(("https://tilo.so".to_owned(), "Tilo".to_owned())))?;

// The real component: controls, permissions, undo and history.
let mut app = TestApp::<App>::new()?;
app.harness().perform_named("url", &Action::SetValue("https://tilo.so".into()))?;
app.harness().click_named("add")?;
assert_eq!(app.state().bookmarks.len(), 1);
assert!(app.undo());
```

`Replica` applies actions through the same door as the host; `TestApp` drives
the production view, roles (`with_role`), actors and undo, and asserts on the
structured history entries (`app.versions()`).
