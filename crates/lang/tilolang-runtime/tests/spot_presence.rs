//! Presence of a place in a document, as a view declares it and reads it:
//! `presence(r, x, y, wide, tall)` says where the person looking works in the document of `r`,
//! and `spots(r)` reads where everyone else is there, in the document's own units, as the
//! platform says it. A view that read either runs again when it moves.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{
        Actor, AmStore, InvokeOutcome, ResourceRef, Runtime, UiNode, Value, ViewInstance,
    };

    /// An app whose one view draws a document and says where its person is in it: their place
    /// is a field of the board, so an action moving it is the person moving.
    const SOURCE: &str = r#"app so.tilo.spots "Spots"

type Board {
    title: Text
    place: List<Float>
}

action move(board: Board, x: Float, y: Float) {
    board.place = [x, y]
}

view board(b: Board) {
    let p = b.place
    presence(b, p.at(0) ?? 0.0, p.at(1) ?? 0.0, 0.0, 0.0)
    ui.Text(value: teammates(b))
}

/// Where the other members are at work here, as the system says: who and where, in the
/// document's own units.
fn teammates(b: Board) -> Text {
    spots(b).map(s -> "{s.who.name()}@{s.x.to_int()},{s.y.to_int()}").join("; ")
}
"#;

    struct Spots {
        rt: Runtime,
        app: App,
        board: ResourceRef,
    }

    impl Spots {
        fn new() -> Self {
            let app = App::compile(SOURCE, &[]);
            let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
            app.load(&rt);
            let board = rt.create("Board").unwrap();
            Self { rt, app, board }
        }

        fn view(&self) -> ViewInstance {
            self.rt
                .open_view(
                    &me(),
                    &self.app.view("board"),
                    vec![Value::resource(&self.board)],
                )
                .unwrap()
        }

        fn move_to(&self, x: f64, y: f64) {
            let id = self.app.action("move");
            let out = self
                .rt
                .invoke_action(
                    &me(),
                    (
                        id.as_str(),
                        vec![
                            Value::resource(&self.board),
                            Value::Float(x),
                            Value::Float(y),
                        ],
                    ),
                )
                .unwrap();
            assert!(matches!(out, InvokeOutcome::Done(_)), "the move ran");
        }

        /// Where each other member is, as the platform says it to the app: `(who, place)`.
        fn say(&self, at: &[(&str, [f64; 4])]) {
            let at: Vec<(Actor, [f64; 4])> = at
                .iter()
                .map(|(who, spot)| {
                    (
                        Actor::from(tilolang_runtime::ids::MemberId::person(*who)),
                        *spot,
                    )
                })
                .collect();
            self.rt.set_spots(&self.board.id_string(), &at);
        }
    }

    fn texts(tree: &UiNode) -> Vec<String> {
        of_kind(tree, "ui.Text")
            .iter()
            .map(|t| text_prop(t, "value"))
            .collect()
    }

    tilo_test_env::scenario! {
        fn spot_presence;

        phase a_view_declares_where_its_person_works_and_says_it_again_when_they_move;
        // The view draws the document and, in one line, says where its person is in it. The
        // host reads that as the person's place and says it to the workspace; when an action
        // moves them, the view runs again and the place it declares moves with it.
        {
            let spots = Spots::new();
            let mut view = spots.view();
            assert_eq!(
                view.declared_spot().map(|(_, at)| at),
                Some([0.0, 0.0, 0.0, 0.0]),
                "a view that declares its place says the one it has"
            );
            assert_eq!(
                view.declared_spot().map(|(target, _)| target.id_string()),
                Some(spots.board.id_string()),
                "and the document it is in"
            );
            spots.move_to(12.0, 34.0);
            let _ = view.poll_patches().unwrap();
            assert_eq!(
                view.declared_spot().map(|(_, at)| at),
                Some([12.0, 34.0, 0.0, 0.0]),
                "the place the view declares is the person's"
            );
            spots.move_to(56.0, 78.0);
            let _ = view.poll_patches().unwrap();
            assert_eq!(
                view.declared_spot().map(|(_, at)| at),
                Some([56.0, 78.0, 0.0, 0.0]),
                "moved where they moved"
            );
        }

        phase a_view_reads_where_everyone_else_is_and_runs_again_as_they_move_and_leave;
        // What the platform says of the other members is what `spots(r)` reads, in the
        // document's own units: a view that read it runs again when one moves, and when one
        // leaves, and shows exactly who is there.
        {
            let spots = Spots::new();
            let mut view = spots.view();
            assert_eq!(texts(view.tree()), [""], "nobody else is here yet");
            spots.say(&[("ben", [10.0, 20.0, 0.0, 0.0])]);
            let _ = view.poll_patches().unwrap();
            assert_eq!(texts(view.tree()), ["ben@10,20"], "Ben is at work here");
            spots.say(&[("ben", [30.0, 40.0, 0.0, 0.0])]);
            let _ = view.poll_patches().unwrap();
            assert_eq!(texts(view.tree()), ["ben@30,40"], "he moved");
            spots.say(&[
                ("ben", [30.0, 40.0, 0.0, 0.0]),
                ("cleo", [5.0, 6.0, 4.0, 2.0]),
            ]);
            let _ = view.poll_patches().unwrap();
            assert_eq!(
                texts(view.tree()),
                ["ben@30,40; cleo@5,6"],
                "two of them are here, in the order the platform lists them"
            );
            spots.say(&[("cleo", [5.0, 6.0, 4.0, 2.0])]);
            let _ = view.poll_patches().unwrap();
            assert_eq!(texts(view.tree()), ["cleo@5,6"], "Ben left");
            spots.say(&[]);
            let _ = view.poll_patches().unwrap();
            assert_eq!(texts(view.tree()), [""], "nobody is here");
        }
    }
}
