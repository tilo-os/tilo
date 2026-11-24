//! A type's `live when` (app-language/search.md, "Live activities"): when an instance is live, read
//! from its own fields, and the `view live` the shell shows of it then.

use crate::common::{codes, compile_with};
use tilolang::pred::{CmpOp, Lit, Operand, Pred};
use tilolang::{parse, print_module};

const PLAYER: &str = r"/// A track the player plays.
type Track {
    title: Text
    state: PlayState = .stopped

    live when state == .playing or state == .paused
}

/// Where a track stands.
enum PlayState { stopped, playing, paused }

/// Pause it.
action pause(t: Track) (icon: .pause) {
    t.state = .paused
}

/// What the bar, quick settings and the lock screen show while it plays.
view live(t: Track) {
    row(gap: .related) {
        ui.Text(t.title)
        button(pause(t))
    }
}
";

fn errors(body: &str) -> String {
    let src = format!("app t.x \"X\"\n{body}\n");
    match compile_with(&src, &[]) {
        Ok(_) => panic!("expected an error for:\n{body}"),
        Err(d) => d
            .into_iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

tilo_test_env::scenario! {
    /// `live when` compiles to the predicate the resource index keeps, names the type's `view
    /// live`, and is refused with a reason a person can act on otherwise.
    fn a_type_says_when_it_is_live_and_the_view_that_shows_it;

    phase the_condition_and_the_view_reach_the_types_meta;
    {
        let compiled = compile_with(&format!("app t.x \"X\"\n{PLAYER}"), &[])
            .unwrap_or_else(|d| panic!("{d:#?}"));
        let track = compiled.meta.types.iter().find(|t| t.name == "Track").expect("Track");
        let state = track.fields.iter().find(|f| f.name == "state").expect("state").id.clone();
        let live = track.live.as_ref().expect("Track says when it is live");
        let cases = &compiled.meta.enums.iter().find(|e| e.name == "PlayState").expect("PlayState").variants;
        let case = |name: &str| cases.iter().find(|v| v.name == name).expect("case").id.clone();
        let is = |name: &str| {
            Pred::Cmp(
                CmpOp::Eq,
                Operand::Field(state.clone()),
                Operand::Lit(Lit::Variant(case(name))),
            )
        };
        assert_eq!(live.when, Pred::Or(vec![is("playing"), is("paused")]));
        let view = compiled.meta.views.iter().find(|v| v.name == "live").expect("view live");
        assert_eq!(live.view, view.id);
        // The predicate answers a record's own values, as the index asks it.
        let playing = live
            .when
            .eval(&mut |f: &String| -> Result<Lit, String> {
                Ok(if *f == state { Lit::Variant(case("playing")) } else { Lit::Absent })
            })
            .expect("it reads");
        assert!(playing);
    }

    phase a_field_called_live_is_still_a_field;
    {
        assert!(codes("type Video {\n    title: Text\n    live: Bool\n}").is_empty());
    }

    phase it_prints_as_it_is_written;
    {
        let src = format!("app t.x \"X\"\n\n{PLAYER}");
        let module = parse(&src).expect("parses");
        let printed = print_module(&module);
        assert!(
            printed.contains("    live when state == .playing or state == .paused\n"),
            "{printed}"
        );
        assert_eq!(print_module(&parse(&printed).expect("reparses")), printed);
    }

    phase a_type_live_with_no_live_view_is_told_how_to_add_one;
    {
        let got = errors("type Timer {\n    running: Bool\n\n    live when running\n}");
        assert!(
            got.contains("E107") && got.contains("add `view live(x: Timer)"),
            "{got}"
        );
        let got = errors(
            "type Timer {\n    running: Bool\n\n    live when running\n}\ntype Other {\n    x: Int\n}\nview live(o: Other) {\n    ui.Text(\"{o.x}\")\n}",
        );
        assert!(
            got.contains("E107") && got.contains("takes the resource and nothing else"),
            "{got}"
        );
    }

    phase a_condition_the_index_cannot_keep_is_refused;
    {
        let cases = [
            ("running and ends < now", "reads the clock"),
            ("owner == workspace.me", "names the person asking"),
            ("title", "not a Bool"),
            ("nope", "`nope` is not a field of `Timer`"),
            ("other.running", "reads another resource"),
            ("ends > started", "compares a field with another field"),
            ("1 == count", "write the field first"),
        ];
        for (when, says) in cases {
            let got = errors(&format!(
                "type Timer {{\n    title: Text\n    running: Bool\n    ends: Time\n    started: Time\n    count: Int\n    owner: Option<Actor> = .none\n    other: Option<Timer>\n\n    live when {when}\n}}\nview live(t: Timer) {{\n    ui.Text(t.title)\n}}"
            ));
            assert!(
                got.contains("E107") && got.contains(says),
                "`live when {when}`: expected E107 saying {says:?}, got:\n{got}"
            );
        }
        let got = errors(
            "type Timer {\n    running: Bool\n\n    live when running\n    live when not running\n}\nview live(t: Timer) {\n    ui.Text(\"x\")\n}",
        );
        assert!(got.contains("says once when it is live"), "{got}");
    }
}
