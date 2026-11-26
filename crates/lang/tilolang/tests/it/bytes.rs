//! `Bytes` as a field's type: the bytes a run holds and a record keeps, written whole, with the
//! length, emptiness and text a value of it reads. Another app's reading and a run's answer are
//! still no stored field.

use crate::common::{codes, compile_body, validate};

tilo_test_env::scenario! {
    /// `Bytes` is a field's type, and its value's surface is the length, emptiness and text.
    fn bytes_are_a_stored_field_type;

    phase a_bytes_field_compiles_and_declares_its_merge;
    {
        let compiled = compile_body(
            "type Run {\n\
             output: Bytes\n\
             }\n\
             action keep(r: Run, data: Bytes) { r.output = data }\n",
        );
        let run = compiled
            .meta
            .types
            .iter()
            .find(|ty| ty.name == "Run")
            .expect("the type declares");
        let output = run
            .fields
            .iter()
            .find(|field| field.name == "output")
            .expect("its field declares");
        assert_eq!(output.ty, "Bytes");
        assert_eq!(output.merge, tilolang::types::MergeKind::Register);
        validate(&compiled.wasm);
    }

    phase the_methods_a_bytes_value_has_are_typed;
    {
        // Each function's return type is the assertion: a method that gave another type would
        // not compile.
        let compiled = compile_body(
            "type Run {\n\
             output: Bytes\n\
             }\n\
             fn spoken(r: Run) -> Text { return r.output.text() }\n\
             fn size(r: Run) -> Int { return r.output.len() }\n\
             fn blank(r: Run) -> Bool { return r.output.is_empty() }\n",
        );
        validate(&compiled.wasm);
    }

    phase bytes_are_written_from_a_value_the_language_makes;
    {
        compile_body(
            "type Run {\n\
             output: Bytes\n\
             }\n\
             action held(r: Run, text: Text) { r.output = Bytes.from_base64(text) }\n",
        );
    }

    phase a_value_of_another_type_is_not_written_as_bytes;
    {
        assert_eq!(
            codes(
                "type Run {\n\
                 output: Bytes\n\
                 }\n\
                 action wrong(r: Run, text: Text) { r.output = text }\n"
            ),
            vec!["E020"],
            "a Text does not stand where Bytes is"
        );
    }

    phase a_value_only_a_run_holds_is_still_no_field;
    {
        for refused in ["Response", "json.Value"] {
            let body = format!("type Run {{\noutput: {refused}\n}}\n");
            assert_eq!(
                codes(&body),
                vec!["E023"],
                "a field cannot hold `{refused}`"
            );
        }
    }
}
