//! Every app under `apps/tl`, compiled with its committed ids and run end to end: chat with
//! embedded agent blocks, letters, the board's foreign views and flow, and every main view at
//! a compact and a wide space.

#[cfg(test)]
#[path = "common/mod.rs"]
mod common;

#[cfg(test)]
mod cases {
    use super::common::*;
    use serde_json::json;
    use tilolang::IdsLock;
    use tilolang_runtime::{
        AmStore, EditOutcome, FlowOutcome, InvokeOutcome, MemorySink, ModuleHash, PackageId,
        Resolution, ResourceRef, Runtime, SimpleAuthority, Space, UiNode, Value, ViewInstance,
    };

    macro_rules! app_source {
        ($name:literal) => {
            (
                include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
                include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
            )
        };
    }

    /// Compile a package with its committed lock, which must already be complete: building
    /// assigns ids to anything new, and a lock that would change is a lock not committed.
    fn build(name: &str, (src, ids): (&str, &str), imports: &[&App]) -> App {
        let lock = IdsLock::parse(ids).unwrap_or_else(|e| panic!("{name}/app.ids: {e}"));
        let metas: Vec<tilolang::PackageMeta> = imports.iter().map(|a| a.meta().clone()).collect();
        let compiled = tilolang::compile_package(src, &lock, &metas)
            .unwrap_or_else(|d| panic!("{name}: {d:#?}"));
        assert_eq!(
            compiled.ids.to_text(),
            ids,
            "apps/tl/{name}/app.ids is out of date: run `tilolang build apps/tl/{name}`"
        );
        App { compiled }
    }

    struct Apps {
        notes: App,
        todo: App,
        chat: App,
        letters: App,
        counter: App,
        board: App,
    }

    fn apps() -> Apps {
        let notes = build("notes", app_source!("notes"), &[]);
        let todo = build("todo", app_source!("todo"), &[]);
        let chat = build("chat", app_source!("chat"), &[]);
        let letters = build("letters", app_source!("letters"), &[]);
        let counter = build("counter", app_source!("counter"), &[]);
        let board = build("board", app_source!("board"), &[&notes, &todo, &chat]);
        Apps {
            notes,
            todo,
            chat,
            letters,
            counter,
            board,
        }
    }

    struct World {
        rt: Runtime,
        auth: SimpleAuthority,
        sink: MemorySink,
        ids: Vec<PackageId>,
    }

    fn world(apps: &Apps, store: &AmStore) -> World {
        let auth = SimpleAuthority::new();
        let rt = Runtime::new(store.clone(), auth.clone()).unwrap();
        let sink = MemorySink::new();
        rt.set_sink(sink.clone());
        let ids = [
            &apps.notes,
            &apps.todo,
            &apps.chat,
            &apps.letters,
            &apps.counter,
            &apps.board,
        ]
        .iter()
        .map(|a| a.load(&rt))
        .collect();
        World {
            rt,
            auth,
            sink,
            ids,
        }
    }

    impl World {
        fn call(&self, app: &App, action: &str, args: Vec<Value>) -> InvokeOutcome {
            let id = app.action(action);
            self.rt.invoke_action(&me(), (id.as_str(), args)).unwrap()
        }

        fn view(&self, app: &App, name: &str, args: Vec<Value>) -> ViewInstance {
            self.rt
                .open_view(&me(), &app.view(name), args)
                .unwrap_or_else(|e| panic!("{name}: {e}"))
        }

        fn create(&self, ty: &str) -> ResourceRef {
            self.rt.create(ty).unwrap()
        }

        fn id_of(&self, which: usize) -> &PackageId {
            &self.ids[which]
        }
    }

    fn res(r: &ResourceRef) -> Value {
        Value::resource(r)
    }

    fn post(
        w: &World,
        apps: &Apps,
        conv: &ResourceRef,
        author: &str,
        text: &str,
        block: &str,
    ) -> ResourceRef {
        returned_ref(w.call(
            &apps.chat,
            "post",
            vec![
                res(conv),
                Value::text(author),
                Value::text(text),
                Value::text(block),
            ],
        ))
    }

    fn start_chat(w: &World, apps: &Apps) -> (ResourceRef, ResourceRef) {
        let chat = w.create("Chat");
        let conv =
            returned_ref(w.call(&apps.chat, "start", vec![res(&chat), Value::text("Plans")]));
        (chat, conv)
    }

    fn items(w: &World, at: &ResourceRef, field: &str) -> Vec<ResourceRef> {
        let Value::List(items) = w.rt.read(at, field).unwrap() else {
            panic!("{field} is not a list");
        };
        items
            .into_iter()
            .map(|v| match v {
                Value::Ref(r) => r.target,
                other => panic!("not a reference: {other:?}"),
            })
            .collect()
    }

    fn text_of(w: &World, at: &ResourceRef, field: &str) -> String {
        w.rt.read(at, field).unwrap().as_text().unwrap().to_owned()
    }

    /// The thread of a conversation: each message's author and its parts as (kind, body).
    fn thread(w: &World, conv: &ResourceRef) -> Vec<(String, Vec<(String, String)>)> {
        items(w, conv, "messages")
            .iter()
            .map(|m| {
                let parts = items(w, m, "parts")
                    .iter()
                    .map(|p| (text_of(w, p, "kind"), text_of(w, p, "body")))
                    .collect();
                (text_of(w, m, "author"), parts)
            })
            .collect()
    }

    fn state(n: &UiNode) -> String {
        text_prop(n, "state")
    }

    fn embeds(tree: &UiNode) -> Vec<&UiNode> {
        of_kind(tree, "embed")
    }

    fn captions(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "text")
            .into_iter()
            .filter(|t| t.props.get("style") == Some(&Value::text("caption")))
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    fn button_labelled<'a>(tree: &'a UiNode, label: &str) -> &'a UiNode {
        of_kind(tree, "button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text(label)))
            .unwrap_or_else(|| panic!("no button `{label}` in\n{}", dump(tree)))
    }

    fn suggested(out: InvokeOutcome) -> tilolang_runtime::Suggestion {
        match out {
            InvokeOutcome::Suggested(s) => s,
            other => panic!("expected a suggestion, got {other:?}"),
        }
    }

    // ----- the apps compile -----------------------------------------------------------------

    #[test]
    fn every_app_compiles_with_its_committed_ids_and_loads_together() {
        let a = apps();
        let w = world(&a, &AmStore::new());
        assert_eq!(w.ids.len(), 6);
        for app in [&a.notes, &a.todo, &a.chat, &a.letters, &a.counter, &a.board] {
            assert!(
                app.meta().views.iter().any(|v| v.name == "main"),
                "{} has no `main` view",
                app.meta().app
            );
        }
    }

    // ----- chat -------------------------------------------------------------------------------

    #[test]
    fn two_chat_replicas_posting_concurrently_converge_in_order_with_authors() {
        let apps = apps();
        let (mut a, mut b) = (AmStore::new(), AmStore::new());
        let (wa, wb) = (world(&apps, &a), world(&apps, &b));
        let (_, conv) = start_chat(&wa, &apps);
        a.sync_with(&mut b).unwrap();

        post(&wa, &apps, &conv, "person:ada", "first", "");
        post(&wa, &apps, &conv, "agent:helper", "second", "1 + 2");
        post(&wb, &apps, &conv, "person:bo", "concurrent", "");
        a.sync_with(&mut b).unwrap();

        let (ta, tb) = (thread(&wa, &conv), thread(&wb, &conv));
        assert_eq!(ta, tb, "both replicas read the same thread");
        assert_eq!(ta.len(), 3);
        let authors: Vec<&str> = ta.iter().map(|(a, _)| a.as_str()).collect();
        let at = |who: &str| authors.iter().position(|a| *a == who).unwrap();
        assert!(
            at("person:ada") < at("agent:helper"),
            "one replica's posts keep their order: {authors:?}"
        );
        assert!(authors.contains(&"person:bo"));
        let helper = &ta[at("agent:helper")].1;
        assert_eq!(
            helper,
            &[
                ("text".to_owned(), "second".to_owned()),
                ("block".to_owned(), "1 + 2".to_owned())
            ],
            "a message keeps its parts, in order"
        );

        let view = wb.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(
            captions(view.tree()),
            authors,
            "the view shows the messages in the converged order, with their authors"
        );
    }

    #[test]
    fn typing_in_the_composer_and_sending_posts_a_message_and_clears_the_entry() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let inputs = of_kind(view.tree(), "input");
        let draft = inputs[1].key.clone();
        assert_eq!(
            view.edit(&draft, &Value::text("hello")).unwrap(),
            EditOutcome::Applied
        );
        let send = button_labelled(view.tree(), "Send").key.clone();
        assert!(matches!(view.press(&send).unwrap(), InvokeOutcome::Done(_)));
        assert_eq!(thread(&w, &conv).len(), 1);
        assert_eq!(text_of(&w, &conv, "draft"), "");
        view.poll_patches().unwrap();
        assert_eq!(captions(view.tree()), ["person"]);
    }

    #[test]
    fn an_agents_block_that_is_a_notes_view_renders_that_view_inline_and_stays_live() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let notebook = w.create("Notebook");
        let note = returned_ref(w.call(&apps.notes, "new_note", vec![res(&notebook)]));
        w.call(
            &apps.notes,
            "retitle",
            vec![res(&note), Value::text("Groceries")],
        );
        let src = format!(
            "notes.editor(@notes.Notebook:{}, @notes.Note:{})",
            notebook.id_string(),
            note.id_string()
        );
        post(&w, &apps, &conv, "agent:helper", "Here is your note", &src);

        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(found.len(), 1, "{}", dump(view.tree()));
        assert_eq!(state(found[0]), "view", "{}", dump(view.tree()));
        let title = of_kind(found[0], "input")[0];
        let binding = title.binding.clone().expect("the title input is bound");
        assert_eq!(binding.target, note);
        assert_eq!(
            &binding.author,
            w.id_of(0),
            "the nested view is the notes package's code, not the agent's"
        );
        assert_eq!(text_prop(title, "placeholder"), "Title");

        let pin = button_labelled(found[0], "Pin").key.clone();
        let out = view.press(&pin).unwrap();
        assert!(
            matches!(out, InvokeOutcome::Done(_)),
            "the notes view's own button is allowed even inside an agent's message: {out:?}"
        );
        assert_eq!(w.rt.read(&note, "pinned").unwrap(), Value::Bool(true));
        view.poll_patches().unwrap();
        button_labelled(embeds(view.tree())[0], "Unpin");
    }

    #[test]
    fn a_block_that_is_a_value_renders_through_the_default_view() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("TodoList");
        w.call(
            &apps.todo,
            "rename",
            vec![res(&list), Value::text("Groceries")],
        );
        post(&w, &apps, &conv, "agent:helper", "", "1 + 2");
        post(
            &w,
            &apps,
            &conv,
            "agent:helper",
            "",
            &format!("@todo.TodoList:{}", list.id_string()),
        );
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(
            found.iter().map(|e| state(e)).collect::<Vec<_>>(),
            ["data", "data"]
        );
        assert_eq!(text_prop(of_kind(found[0], "text")[0], "value"), "3");
        assert_eq!(
            text_prop(of_kind(found[1], "heading")[0], "value"),
            "Groceries"
        );
    }

    #[test]
    fn a_block_that_does_not_compile_renders_its_diagnostics_and_the_chat_goes_on() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "agent:helper", "see", "todo.nothing(1)");
        post(&w, &apps, &conv, "person:ada", "still here", "");
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(state(found[0]), "error", "{}", dump(view.tree()));
        let error = of_kind(found[0], "error")[0];
        assert!(!error.children.is_empty());
        let first = text_prop(&error.children[0], "value");
        assert!(
            first.contains("E0"),
            "the diagnostic keeps its code: {first}"
        );
        assert_eq!(captions(view.tree()), ["agent:helper", "person:ada"]);
    }

    #[test]
    fn the_same_block_source_is_compiled_once() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let before = w.rt.compiled_modules();
        for who in ["agent:a", "agent:b", "person:c"] {
            post(&w, &apps, &conv, who, "", "40 + 2");
        }
        let view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(embeds(view.tree()).len(), 3);
        assert_eq!(
            w.rt.compiled_modules(),
            before + 1,
            "three embeds of one source are one module"
        );
        drop(view);
        let _again = w.view(&apps.chat, "thread", vec![res(&conv)]);
        assert_eq!(
            w.rt.compiled_modules(),
            before + 1,
            "and re-rendering adds none"
        );
    }

    #[test]
    fn a_block_applying_letters_send_is_suggested_and_confirming_it_delivers_the_letter() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let desk = w.create("Desk");
        let draft = returned_ref(w.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let mut editor = w.view(&apps.letters, "editor", vec![res(&desk), res(&draft)]);
        let inputs: Vec<String> = of_kind(editor.tree(), "input")
            .iter()
            .map(|i| i.key.clone())
            .collect();
        for (key, text) in inputs
            .iter()
            .zip(["bo@example.org", "Plans", "See you at nine."])
        {
            assert_eq!(
                editor.edit(key, &Value::text(text)).unwrap(),
                EditOutcome::Applied
            );
        }
        editor.poll_patches().unwrap();

        // The person's own Send button also asks: an outside effect always does.
        let own = button_labelled(editor.tree(), "Send").key.clone();
        let own = suggested(editor.press(&own).unwrap());
        assert_eq!(own.chain.len(), 1);

        let src = format!("letters.send(@letters.Draft:{})", draft.id_string());
        post(&w, &apps, &conv, "agent:helper", "Shall I send it?", &src);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(view.tree());
        assert_eq!(state(found[0]), "invocation", "{}", dump(view.tree()));
        let inv = of_kind(found[0], "invocation")[0];
        assert_eq!(text_prop(inv, "state"), "suggested");
        assert_eq!(text_prop(inv, "action"), "send");
        assert_eq!(text_prop(inv, "app"), "so.tilo.letters");
        let run = text_prop(inv, "run");
        assert_eq!(
            text_prop(of_kind(inv, "heading")[0], "value"),
            "Send to bo@example.org",
            "the proposal view shows the letter: {}",
            dump(inv)
        );
        assert!(w.rt.pending_outbox().is_empty(), "asking sends nothing");

        // A re-render shows the same suggestion rather than making another.
        assert!(view.set_space("", Space::from_rem(20.0, 40.0)));
        view.poll_patches().unwrap();
        let again = of_kind(view.tree(), "invocation")[0];
        assert_eq!(text_prop(again, "run"), run);

        let out = w.rt.resolve(&run, &me(), Resolution::Approve).unwrap();
        assert!(matches!(out, FlowOutcome::Done(_)), "{out:?}");
        assert!(
            view.is_stale(),
            "deciding a suggestion refreshes the message"
        );
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "invocation")[0], "state"),
            "done"
        );
        assert!(
            of_kind(view.tree(), "heading")
                .iter()
                .all(|h| text_prop(h, "value") != "Send to bo@example.org"),
            "a decided suggestion no longer shows its proposal"
        );

        let pending = w.rt.pending_outbox();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].capability, "post");
        assert_eq!(
            pending[0].payload,
            json!(["bo@example.org", "Plans", "See you at nine."])
        );
        assert_eq!(w.rt.read(&draft, "sent").unwrap(), Value::Bool(true));
        assert_eq!(w.rt.settle().unwrap().delivered, 1);
        assert_eq!(
            w.sink.delivered(),
            vec![(
                "post".to_owned(),
                json!(["bo@example.org", "Plans", "See you at nine."])
            )]
        );
        view.poll_patches().unwrap();
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn declining_a_suggested_block_shows_declined_and_sends_nothing() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let desk = w.create("Desk");
        let draft = returned_ref(w.call(&apps.letters, "new_draft", vec![res(&desk)]));
        let src = format!("letters.send(@letters.Draft:{})", draft.id_string());
        post(&w, &apps, &conv, "agent:helper", "", &src);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let run = text_prop(of_kind(view.tree(), "invocation")[0], "run");
        w.rt.resolve(&run, &me(), Resolution::Decline).unwrap();
        view.poll_patches().unwrap();
        assert_eq!(
            text_prop(of_kind(view.tree(), "invocation")[0], "state"),
            "declined"
        );
        assert!(w.rt.pending_outbox().is_empty());
    }

    #[test]
    fn a_strangers_block_asks_for_a_foreign_action_that_the_apps_own_view_may_run() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("TodoList");
        let first = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("milk")]));
        let second = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("eggs")]));
        let remove = |item: &ResourceRef| {
            format!(
                "button(\"Remove\", on: todo.remove(@todo.TodoList:{}, @todo.Todo:{}))",
                list.id_string(),
                item.id_string()
            )
        };
        post(&w, &apps, &conv, "person:stranger", "", &remove(&first));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        assert_eq!(state(found[0]), "view");
        let button = of_kind(found[0], "button")[0].key.clone();
        let s = suggested(chat_view.press(&button).unwrap());
        assert_eq!(s.action, apps.todo.action("remove"));
        assert_eq!(
            s.chain,
            vec![ModuleHash::principal("person:stranger")],
            "the chain names the principal who wrote the block"
        );
        assert_eq!(items(&w, &list, "items").len(), 2, "asking changed nothing");

        // The same action from the todo app's own view runs without asking.
        let own = w.view(&apps.todo, "main", vec![res(&list)]);
        let own_remove = of_kind(own.tree(), "button")
            .into_iter()
            .find(|b| b.props.get("label") == Some(&Value::text("Remove")))
            .unwrap()
            .key
            .clone();
        let out = own.press(&own_remove).unwrap();
        assert!(matches!(out, InvokeOutcome::Done(_)), "{out:?}");
        assert_eq!(items(&w, &list, "items"), vec![second.clone()]);

        // Trusting the principal covers every block it writes, from the next one on.
        w.auth.trust_principal("person:stranger");
        let third = returned_ref(w.call(&apps.todo, "add", vec![res(&list), Value::text("bread")]));
        post(&w, &apps, &conv, "person:stranger", "", &remove(&third));
        let chat_view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let found = embeds(chat_view.tree());
        let button = of_kind(found[1], "button")[0].key.clone();
        assert!(matches!(
            chat_view.press(&button).unwrap(),
            InvokeOutcome::Done(_)
        ));
        assert_eq!(items(&w, &list, "items"), vec![second]);
        w.auth.revoke_principal("person:stranger");
    }

    #[test]
    fn a_trusted_agents_action_block_runs_once_however_often_the_message_renders() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (_, conv) = start_chat(&w, &apps);
        let list = w.create("TodoList");
        w.auth.trust_principal("agent:helper");
        let src = format!("todo.add(@todo.TodoList:{}, \"bread\")", list.id_string());
        post(&w, &apps, &conv, "agent:helper", "", &src);
        let mut view = w.view(&apps.chat, "thread", vec![res(&conv)]);
        let inv = of_kind(view.tree(), "invocation")[0];
        assert_eq!(text_prop(inv, "state"), "done");
        assert_eq!(items(&w, &list, "items").len(), 1);
        for width in [20.0, 90.0, 20.0] {
            view.set_space("", Space::from_rem(width, 40.0));
            view.poll_patches().unwrap();
        }
        let again = w.view(&apps.chat, "thread", vec![res(&conv)]);
        drop(again);
        assert_eq!(
            items(&w, &list, "items").len(),
            1,
            "rendering again repeats nothing"
        );
    }

    // ----- board ------------------------------------------------------------------------------

    struct Morning {
        w: World,
        board: ResourceRef,
        list: ResourceRef,
        notebook: ResourceRef,
    }

    fn morning(apps: &Apps) -> Morning {
        let w = world(apps, &AmStore::new());
        let board = w.create("Board");
        let notebook = w.create("Notebook");
        let list = w.create("TodoList");
        let chat = w.create("Chat");
        w.call(
            &apps.board,
            "attach",
            vec![res(&board), res(&notebook), res(&list), res(&chat)],
        );
        Morning {
            w,
            board,
            list,
            notebook,
        }
    }

    fn add_todo(m: &Morning, apps: &Apps, title: &str, done: bool) {
        let item =
            returned_ref(m.w.call(&apps.todo, "add", vec![res(&m.list), Value::text(title)]));
        if done {
            m.w.call(&apps.todo, "toggle", vec![res(&item)]);
        }
    }

    #[test]
    fn the_boards_views_apply_other_packages_views() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "milk", false);
        m.w.call(&apps.notes, "new_note", vec![res(&m.notebook)]);
        let view = m.w.view(&apps.board, "main", vec![res(&m.board)]);
        let tree = view.tree();
        assert_eq!(
            of_kind(tree, "heading")
                .iter()
                .map(|h| text_prop(h, "value"))
                .collect::<Vec<_>>(),
            ["Today", "Todo (1 open)", "Notes", "Chats"],
            "{}",
            dump(tree)
        );
        assert_eq!(of_kind(tree, "checkbox").len(), 1, "the todo app's rows");
    }

    #[test]
    fn the_morning_cleanup_asks_for_every_foreign_change_and_replays_without_repeating() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "a", true);
        add_todo(&m, &apps, "b", false);
        add_todo(&m, &apps, "c", true);

        let view = m.w.view(&apps.board, "main", vec![res(&m.board)]);
        let button = button_labelled(view.tree(), "Morning cleanup").key.clone();
        let mut out = FlowOutcome::Suspended(suggested(view.press(&button).unwrap()));
        let mut asked = Vec::new();
        let finished = loop {
            match out {
                FlowOutcome::Suspended(s) => {
                    assert_eq!(
                        s.chain,
                        vec![m.w.id_of(5).clone()],
                        "the board wrote these invocations"
                    );
                    asked.push(s.action.clone());
                    out = m.w.rt.resolve(&s.run, &me(), Resolution::Approve).unwrap();
                }
                FlowOutcome::Done(v) => break v,
            }
        };
        assert_eq!(finished, Value::Int(2));
        assert_eq!(
            asked,
            [
                apps.todo.action("remove"),
                apps.todo.action("remove"),
                apps.notes.action("new_note"),
                apps.notes.action("retitle"),
                apps.notes.action("tag"),
            ],
            "one question per foreign action, in flow order"
        );
        assert_eq!(items(&m.w, &m.list, "items").len(), 1);
        let notes = items(&m.w, &m.notebook, "notes");
        assert_eq!(notes.len(), 1, "replays did not file a second note");
        assert_eq!(text_of(&m.w, &notes[0], "title"), "Morning: 2 done");
        assert_eq!(
            m.w.rt.read(&notes[0], "tags").unwrap(),
            Value::List(vec![Value::text("morning")])
        );
        let removes =
            m.w.rt
                .history(&m.list.doc)
                .unwrap()
                .iter()
                .filter(|n| n.action == apps.todo.action("remove"))
                .count();
        assert_eq!(removes, 2);
    }

    #[test]
    fn a_trusted_board_runs_the_cleanup_without_asking() {
        let apps = apps();
        let m = morning(&apps);
        add_todo(&m, &apps, "a", true);
        m.w.auth.trust(m.w.id_of(5));
        let flow = apps.board.flow("morning_cleanup");
        let out =
            m.w.rt
                .run_flow(&me(), &flow, vec![res(&m.list), res(&m.notebook)])
                .unwrap();
        assert_eq!(out, FlowOutcome::Done(Value::Int(1)));
        assert!(items(&m.w, &m.list, "items").is_empty());
    }

    // ----- every app at every width -------------------------------------------------------------

    fn at(v: &mut ViewInstance, width: f64) -> UiNode {
        v.set_space("", Space::from_rem(width, 40.0));
        v.poll_patches().unwrap();
        v.tree().clone()
    }

    const COMPACT: f64 = 24.0;
    const WIDE: f64 = 90.0;

    fn differ(compact: &UiNode, wide: &UiNode) {
        assert_ne!(
            dump(compact),
            dump(wide),
            "the layout reads `space`, so the trees differ"
        );
    }

    #[test]
    fn counter_stacks_when_compact_and_lines_up_when_wide() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let c = w.create("Counter");
        let mut v = w.view(&apps.counter, "main", vec![res(&c)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert_eq!(of_kind(&compact, "page")[0].children[0].kind, "stack");
        assert_eq!(of_kind(&wide, "page")[0].children[0].kind, "row");
        let plus = button_labelled(&wide, "+1").key.clone();
        v.press(&plus).unwrap();
        v.poll_patches().unwrap();
        assert_eq!(w.rt.read(&c, "value").unwrap(), Value::Int(1));
        assert_eq!(text_prop(of_kind(v.tree(), "heading")[0], "value"), "1");
    }

    #[test]
    fn todo_is_a_column_when_compact_and_a_grid_when_wide() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("TodoList");
        for t in ["a", "b"] {
            w.call(&apps.todo, "add", vec![res(&list), Value::text(t)]);
        }
        let mut v = w.view(&apps.todo, "main", vec![res(&list)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert!(of_kind(&compact, "grid").is_empty());
        assert_eq!(
            of_kind(&wide, "grid")[0].props["min.rem"],
            Value::Float(18.0)
        );
        assert_eq!(of_kind(&compact, "input").len(), 1);
        assert_eq!(of_kind(&wide, "input").len(), 2);
        assert_eq!(of_kind(&compact, "checkbox").len(), 2);
        assert_eq!(of_kind(&wide, "checkbox").len(), 2);
    }

    #[test]
    fn typing_an_item_and_adding_it_appends_and_clears_the_entry() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let list = w.create("TodoList");
        let mut v = w.view(&apps.todo, "main", vec![res(&list)]);
        let entry = of_kind(v.tree(), "input")[0].key.clone();
        v.edit(&entry, &Value::text("bread")).unwrap();
        let add = button_labelled(v.tree(), "Add").key.clone();
        assert!(matches!(v.press(&add).unwrap(), InvokeOutcome::Done(_)));
        v.poll_patches().unwrap();
        assert_eq!(of_kind(v.tree(), "checkbox").len(), 1);
        assert_eq!(text_of(&w, &list, "draft"), "");
    }

    #[test]
    fn notes_hide_the_body_preview_when_compact() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let nb = w.create("Notebook");
        w.call(&apps.notes, "new_note", vec![res(&nb)]);
        let mut v = w.view(&apps.notes, "main", vec![res(&nb)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert!(of_kind(&compact, "text").len() < of_kind(&wide, "text").len());
        assert_eq!(
            of_kind(&wide, "split")[0].props["side"],
            Value::text("leading")
        );
    }

    #[test]
    fn letters_hide_the_recipient_when_compact() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let desk = w.create("Desk");
        w.call(&apps.letters, "new_draft", vec![res(&desk)]);
        let mut v = w.view(&apps.letters, "main", vec![res(&desk)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert!(captions(&compact).is_empty());
        assert_eq!(captions(&wide).len(), 1);
        assert_eq!(
            of_kind(&wide, "split")[0].props["collapse"],
            Value::text("navigate")
        );
    }

    #[test]
    fn chat_puts_the_author_beside_the_message_only_when_there_is_room() {
        let apps = apps();
        let w = world(&apps, &AmStore::new());
        let (chat, conv) = start_chat(&w, &apps);
        post(&w, &apps, &conv, "person:ada", "hello", "");
        let mut v = w.view(&apps.chat, "main", vec![res(&chat)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        let author_row = |tree: &UiNode| {
            of_kind(tree, "text")
                .into_iter()
                .find(|t| t.props.get("style") == Some(&Value::text("caption")))
                .map(|t| t.props.get("min.rem").cloned())
        };
        assert_eq!(author_row(&compact), Some(None));
        assert_eq!(author_row(&wide), Some(Some(Value::Float(8.0))));
        assert_eq!(of_kind(&wide, "split").len(), 1);
    }

    #[test]
    fn board_is_a_stack_when_compact_and_a_grid_when_wide() {
        let apps = apps();
        let m = morning(&apps);
        let mut v = m.w.view(&apps.board, "main", vec![res(&m.board)]);
        let (compact, wide) = (at(&mut v, COMPACT), at(&mut v, WIDE));
        differ(&compact, &wide);
        assert!(of_kind(&compact, "grid").is_empty());
        let grid = of_kind(&wide, "grid")[0];
        assert_eq!(grid.props["min.rem"], Value::Float(22.0));
        assert_eq!(grid.children.len(), 3, "one cell per app");
        assert_eq!(grid.children[0].props["span"], Value::Int(2));
        assert_eq!(
            of_kind(&compact, "heading").len(),
            4,
            "the same four panels in both"
        );
    }
}
