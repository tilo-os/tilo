//! `std.sha256` and `std.blake3`, run: the digest of a text in lowercase hex, as a program
//! checks text it must not keep.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::{AmStore, InvokeOutcome, Runtime, Value};

    const SOURCE: &str = "app so.tilo.digest \"Digest\"\n\
action sha(t: Text) -> Text { return std.sha256(t) }\n\
action blake(t: Text) -> Text { return std.blake3(t) }\n\
fn twice(t: Text) -> Text { return std.sha256(std.sha256(t)) }\n\
action check(t: Text) -> Text { return twice(t) }";

    struct Digests {
        rt: Runtime,
        app: App,
    }

    fn digests() -> Digests {
        let app = App::compile(SOURCE, &[]);
        let rt = Runtime::new(AmStore::new(), TestAuthority::new()).unwrap();
        app.load(&rt);
        Digests { rt, app }
    }

    impl Digests {
        fn ask(&self, action: &str, args: Vec<Value>) -> Value {
            let aref = (self.app.action(action), args);
            match self.rt.invoke_action(&me(), aref).unwrap() {
                InvokeOutcome::Done(v) => v,
                InvokeOutcome::Asked(call) => panic!("{action} stopped to ask {call}"),
                InvokeOutcome::Denied => panic!("{action} was denied"),
                InvokeOutcome::Waits { .. } => panic!("{action} stopped to wait on a call"),
            }
        }

        fn text(&self, action: &str, text: &str) -> String {
            match self.ask(action, vec![Value::text(text)]) {
                Value::Text(t) => t,
                other => panic!("{action} gave {other:?}"),
            }
        }
    }

    tilo_test_env::scenario! {
        fn a_text_is_digested_as_the_hash_defines_it;

        phase sha256_answers_the_published_vectors;
        {
            let d = digests();
            assert_eq!(
                d.text("sha", ""),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            );
            assert_eq!(
                d.text("sha", "abc"),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            );
            // A digest is text, so one can be digested again: an offer keeps the digest of the
            // proof, never the proof.
            assert_eq!(
                d.text("check", "abc"),
                "dfe7a23fefeea519e9bbfdd1a6be94c4b2e4529dd6b7cbea83f9959c2621b13c"
            );
        }

        phase blake3_answers_the_published_vectors;
        {
            let d = digests();
            assert_eq!(
                d.text("blake", ""),
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
            );
            assert_eq!(
                d.text("blake", "abc"),
                "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85"
            );
        }
    }
}
