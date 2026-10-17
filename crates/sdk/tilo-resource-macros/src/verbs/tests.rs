use super::*;

#[test]
fn parameter_roles_keep_defaults_and_order_independent_precedence() {
    for (attributes, expected) in [
        ("", Role::Plain),
        ("secret", Role::Plain),
        ("place", Role::Place),
        ("flatten", Role::Flatten),
        ("content", Role::Content),
        ("place, flatten", Role::Flatten),
        ("flatten, place", Role::Flatten),
        ("content, flatten, place", Role::Content),
        ("place, flatten, content", Role::Content),
        ("flatten, content, place", Role::Content),
        ("content, content", Role::Content),
    ] {
        let Ok(options) = syn::parse_str::<ParamOptions>(attributes) else {
            panic!("parameter options did not parse: {attributes}");
        };
        assert_eq!(options.role, expected, "{attributes}");
        assert_eq!(options.secret, attributes == "secret");
        assert!(options.place_in.is_none());
    }
}

#[test]
fn content_parameter_is_marked_in_generated_declaration() {
    let attr: Attribute = syn::parse_quote!(#[action]);
    let mut method = syn::parse_quote! {
        fn attach(&mut self, #[param(content)] body: String) {}
    };
    let verb = read_verb(&mut method, &attr).expect("content action parses");
    assert_eq!(verb.params.len(), 1);
    let content = &verb.params[0];
    assert_eq!(content.role, Role::Content);
    let expected = quote! {
        tilo_resource::__private::ParamDecl {
            name: "body",
            ty: tilo_resource::__private::ParamTypeOf::Known(tilo_resource::__private::ParamType::Text),
            description: "Body",
            schema: <String as tilo_resource::JsonSchema>::json_schema,
            optional: false,
            place: tilo_resource::__private::PlaceIn::Nowhere,
            example: ::core::option::Option::None,
            flat: false,
            content: true,
        }
    };
    assert_eq!(
        param_decl(content, &krate()).to_string(),
        expected.to_string()
    );
}

#[test]
fn read_verb_preserves_parameter_metadata_and_secret_validation() {
    let attr: Attribute = syn::parse_quote!(#[action]);
    let mut method = syn::parse_quote! {
        fn edit(&mut self, note_text: String,
            #[param("Access token", name = "token", secret)] credential: Option<String>) {}
    };
    let Ok(verb) = read_verb(&mut method, &attr) else {
        panic!("action with plain and secret parameters did not parse");
    };
    let mut params = verb.params.iter();
    let Some(plain) = params.next() else {
        panic!("missing plain parameter");
    };
    assert_eq!(plain.public, "note_text");
    assert_eq!(plain.description, humanize("note_text"));
    assert_eq!(plain.role, Role::Plain);
    assert!(!plain.secret);
    assert!(plain.example.is_none());
    let expected = quote! {
        tilo_resource::__private::ParamDecl {
            name: "note_text",
            ty: tilo_resource::__private::ParamTypeOf::Known(tilo_resource::__private::ParamType::Text),
            description: "Note text",
            schema: <String as tilo_resource::JsonSchema>::json_schema,
            optional: false,
            place: tilo_resource::__private::PlaceIn::Nowhere,
            example: ::core::option::Option::None,
            flat: false,
            content: false,
        }
    };
    assert_eq!(
        param_decl(plain, &krate()).to_string(),
        expected.to_string()
    );
    let Some(secret) = params.next() else {
        panic!("missing secret parameter");
    };
    assert_eq!(secret.public, "token");
    assert_eq!(secret.description, "Access token");
    assert_eq!(secret.role, Role::Plain);
    assert!(secret.secret);
    let declaration = param_decl(secret, &krate()).to_string();
    assert!(declaration.contains("ParamType :: Secret"));
    assert!(declaration.contains("optional : true"));
    assert!(declaration.contains("content : false"));
    for attributes in [
        "secret, content",
        "secret, flatten",
        "secret, place",
        "secret, example = \"visible\"",
    ] {
        let Ok(options) = syn::parse_str::<ParamOptions>(attributes) else {
            panic!("parameter options did not parse: {attributes}");
        };
        let param = Param {
            role: options.role,
            example: options.example,
            ..item_param(&syn::parse_quote!(value))
        };
        let param = Param {
            ty: syn::parse_quote!(String),
            secret: true,
            ..param
        };
        assert!(
            check_params(&[param], Span::call_site()).is_err(),
            "{attributes}"
        );
    }
}
