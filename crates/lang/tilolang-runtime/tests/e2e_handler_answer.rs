//! A view handler that binds the answer an action gives: the box an action makes is the view's
//! `editing`, so the next thing the person types lands in that box, and a refused action runs
//! none of the steps after the binding.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        AmStore, InvokeOutcome, ResourceRef, Runtime, SimpleAuthority, UiNode, Value,
    };

    const SOURCE: &str = r#"app t.slides "Slides"

type Shape {
    text: Text
}

type Slide {
    shapes: Collection<Shape>
}

/// Adds a box to the slide and answers its id.
action add_shape(s: Slide) -> Text {
    let made = s.shapes.insert(Shape(text: ""))
    return made.id()
}

/// Adds a box only where the slide holds none yet: a handler that binds this one's answer runs
/// none of its steps when it is refused.
action add_first_shape(s: Slide) -> Text {
    require s.shapes.is_empty() "The slide has a box already"
    let made = s.shapes.insert(Shape(text: ""))
    return made.id()
}

view editor(s: Slide) {
    state editing: Text = ""
    stack {
        ui.Button("Add box") {
            on press: {
                let id = add_shape(s)
                set(editing, id)
            }
        } as add
        ui.Button("Add first box") {
            on press: {
                let id = add_first_shape(s)
                set(editing, id)
            }
        } as add_first
        for shape in s.shapes {
            if shape.id() == editing {
                input(bind: shape.text, placeholder: "Type here")
            }
        }
    }
}
"#;

    struct Editor {
        rt: Runtime,
        app: App,
        slide: ResourceRef,
    }

    impl Editor {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            app.load(&rt);
            let slide = rt.create("Slide").unwrap();
            Self { rt, app, slide }
        }

        fn open(&self) -> tilolang_runtime::ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("editor"),
                    vec![Value::resource(&self.slide)],
                )
                .unwrap()
        }

        /// The slide's boxes, in order.
        fn shapes(&self) -> Vec<ResourceRef> {
            let Value::List(shapes) = self.rt.read(&self.slide, "shapes").unwrap() else {
                panic!("a slide's boxes are a collection");
            };
            shapes
                .iter()
                .map(|shape| match shape {
                    Value::Ref(shape) => (*shape.target).clone(),
                    other => panic!("a box reads as its reference, not {other:?}"),
                })
                .collect()
        }

        fn text_of(&self, shape: &ResourceRef) -> String {
            match self.rt.read(shape, "text").unwrap() {
                Value::Text(text) => String::from(&*text),
                other => panic!("a box's text reads {other:?}"),
            }
        }

        fn editing(&self, view: &tilolang_runtime::ViewInstance) -> Option<String> {
            match view.view_state("editing") {
                Some(Value::Text(text)) => Some(String::from(&*text)),
                Some(other) => panic!("editing reads {other:?}"),
                None => None,
            }
        }
    }

    fn inputs(view: &tilolang_runtime::ViewInstance) -> Vec<UiNode> {
        of_kind(view.tree(), "ui.TextInput")
            .into_iter()
            .cloned()
            .collect()
    }

    tilo_test_env::scenario! {
        fn a_handler_sets_the_view_to_the_answer_an_action_gave;

        phase the_box_an_action_made_is_the_one_the_next_key_lands_in;
        {
            let e = Editor::new();
            let mut view = e.open();
            assert!(inputs(&view).is_empty(), "{}", dump(view.tree()));

            let add = e.app.anchor("editor", "add");
            assert!(matches!(view.press(&add).unwrap(), InvokeOutcome::Done(_)));
            view.poll_patches().unwrap();

            let shapes = e.shapes();
            assert_eq!(shapes.len(), 1, "{}", dump(view.tree()));
            let id = shapes[0].id_string();
            assert_eq!(e.editing(&view).as_deref(), Some(id.as_str()));

            // The box the action made is the one the view edits now.
            let typed = inputs(&view);
            assert_eq!(typed.len(), 1, "{}", dump(view.tree()));
            assert!(matches!(
                view.edit(&typed[0].key, &Value::text("Title")).unwrap(),
                tilolang_runtime::EditOutcome::Applied
            ));
            view.poll_patches().unwrap();
            assert_eq!(e.text_of(&shapes[0]), "Title");
        }

        phase a_refused_action_runs_none_of_the_steps_after_the_binding;
        {
            let e = Editor::new();
            let mut view = e.open();
            let add_first = e.app.anchor("editor", "add_first");
            assert!(matches!(
                view.press(&add_first).unwrap(),
                InvokeOutcome::Done(_)
            ));
            view.poll_patches().unwrap();
            let shapes = e.shapes();
            assert_eq!(shapes.len(), 1);
            let id = shapes[0].id_string();
            assert_eq!(e.editing(&view).as_deref(), Some(id.as_str()));

            // The second box is refused: the view keeps the first one it was set to, and no
            // box is made.
            let refused = view.press(&add_first).expect_err("refused");
            assert!(
                format!("{refused}").contains("The slide has a box already"),
                "{refused}"
            );
            assert_eq!(e.shapes().len(), 1);
            assert_eq!(e.editing(&view).as_deref(), Some(id.as_str()));
        }
    }
}
