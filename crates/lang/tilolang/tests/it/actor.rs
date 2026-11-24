//! `Actor`: values come from the host, never from text.

use crate::common::{codes, compile_body, validate};

const MESSAGE: &str = "type Part {\n    body: Text\n    invocation: Option<host.Invocation>\n}\n\
type Message {\n    author: Actor\n    parts: Collection<Part>\n}\n\
type Room {\n    messages: Collection<Message>\n}\n";

tilo_test_env::scenario! {
    /// `Actor`: values come from the host, never from text.
    fn a_actor_comes_from_the_host_and_never_from_text;

    phase an_action_records_the_invoker_and_compares_actors;
    {
        let c = compile_body(&format!(
            "{MESSAGE}action post(r: Room, text: Text) {{\n    let m = r.messages.insert(Message(author: invoker))\n    let p = m.parts.insert(Part(body: text))\n    run_block(p, text)\n}}\n\
fn mine(m: Message, p: Actor) -> Bool {{\n    return m.author == p\n}}\n\
view v(m: Message) {{\n    text(m.author.name())\n    text(\"by {{m.author}}\")\n}}"
        ));
        validate(&c.wasm);
        let post = c
            .meta
            .actions
            .iter()
            .find(|a| a.name == "post")
            .expect("post");
        assert_eq!(post.class, tilolang::types::EffectClass::Undoable);
        assert_eq!(c.meta.fns[0].params[1].ty, "Actor");
    }

    phase nothing_in_the_language_makes_a_actor_from_text;
    {
        assert_eq!(
            codes(&format!(
                "{MESSAGE}action post(r: Room) {{\n    r.messages.insert(Message(author: \"agent:x\"))\n}}"
            )),
            vec!["E020"],
            "text is not an actor"
        );
        assert!(
            !codes("fn p() -> Actor {\n    return Actor(\"agent:x\")\n}").is_empty(),
            "there is no constructor"
        );
        assert!(
            !codes("fn p(t: Text) -> Actor {\n    return t\n}").is_empty(),
            "and no conversion"
        );
    }

    phase an_action_and_a_view_read_the_invoker;
    {
        assert!(codes("view v() {\n    text(invoker.name())\n}").is_empty());
        assert_eq!(
            codes("fn f() -> Actor {\n    return invoker\n}"),
            vec!["E049"]
        );
    }

    phase a_requirement_reads_the_invoker_who_would_invoke_it_where_it_is_offered;
    {
        assert!(
            codes(
                "type Note {\n    text: Text\n}\naction edit(n: Note, text: Text) {\n    require created(n)?.who == .some(invoker) \"Only its author\"\n    n.text = text\n}"
            )
            .is_empty(),
            "a requirement on who invokes the action"
        );
    }

    phase actions_views_and_fns_take_a_actor;
    {
        assert!(
            codes("action a(p: Actor) {\n}").is_empty(),
            "a person the action acts on; who acts is `invoker`"
        );
        assert!(codes("fn f(p: Actor) -> Actor {\n    return p\n}").is_empty());
        assert!(codes("view v(p: Actor) {\n    text(p.name())\n}").is_empty());
    }

    phase run_block_needs_a_part_with_an_invocation_field;
    {
        assert_eq!(
            codes(
                "type P {\n    body: Text\n}\ntype R {\n    ps: Collection<P>\n}\naction a(r: R) {\n    let p = r.ps.insert(P(body: \"x\"))\n    run_block(p, \"x\")\n}"
            ),
            vec!["E020"]
        );
        assert_eq!(
            codes(&format!(
                "{MESSAGE}fn f(p: Part) -> Int {{\n    run_block(p, \"x\")\n    return 1\n}}"
            )),
            vec!["E047"],
            "only an action may queue a block"
        );
    }
}
