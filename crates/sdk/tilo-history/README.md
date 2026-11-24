# tilo-history

History entries as sentences, in the language of the person reading them. A change in Tilo is
recorded as data: what kind of resource it touched, whether it was a field edit or an action,
the field, the old and new values, and the item it was about. This crate turns one such entry
(`tilo_actions::ActionContext`) into the line a history panel shows; who did it is named beside
that line.

An app supplies its own words and this crate uses them first, by convention: a field edit reads
through the Fluent message `<app>-<type>-<field>`, an action through `<app>-<action>`, with the
item, the old and the new value, a count and the actor as arguments. When the app has no message
for an entry, a framework message says the same thing, naming the field by the label the app
publishes rather than by an id or a path. The same entry reads in German for a German member and
in English for an English one, whoever made the change.

The session's History uses this crate to draw what happened in a workspace. A tool that lists
changes for a person can use it the same way; apps depend on it only when they render history
entries themselves, since declaring an action's label and "did" text is enough for history to
read well.

## Use it

```toml
[dependencies]
tilo-history = "0.1"
tilo-actions = "0.1"
```

```rust
use tilo_actions::{ActionContext, ActionKey, Described, Did, Scalar};
use tilo_history::Reader;

let entry = ActionContext::for_resource("lib", "notes.note", "Notes")
    .with_app("org.example.notes", "Notes")
    .with_described(Described::new(Did::verb(
        ActionKey::new("org.example.notes", "notes.note", "pin"),
        [("title".to_string(), Scalar::from("Groceries"))],
    )));

let reader = Reader::new("de-DE");
println!("{}", reader.sentence(&entry));
```

`Reader::new` takes a locale tag (`de-DE`, `en_US.UTF-8`). `Reader::with_app` gives it the app's
`Messages`, which a `Catalogue` built from the app's Fluent sources provides, and
`Reader::with_labels` gives it the field and action labels the app publishes. Without an app
catalogue the framework's own messages answer, so nothing is left empty.

`Reader::actor` renders the line that names who did something: "You" for the person reading,
the actor's name otherwise, and the task it worked for when the change came through one.
`window_language` chooses which of the languages a window ships to speak, from the person's
preference.

## Learn more

- Rust app documentation: https://tilolang.dev/docs/rust-apps
- Repository: https://code.tilo.so/tilo/tilo

## License

MIT
