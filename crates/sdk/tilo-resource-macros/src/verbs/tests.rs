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
    let declaration = param_decl(content, &krate()).to_string();
    assert!(declaration.contains("name : \"body\""), "{declaration}");
    assert!(declaration.contains("content : true"), "{declaration}");
}

#[test]
fn a_parameter_picking_a_resource_declares_the_picked_reference() {
    let attr: Attribute = syn::parse_quote!(#[action]);
    let mut method = syn::parse_quote! {
        fn link(&mut self, #[param("The note", resource = "notes.notebook.pages")] note: ResourceAt) {}
    };
    let verb = read_verb(&mut method, &attr).expect("picking action parses");
    let note = &verb.params[0];
    assert_eq!(note.picks.as_deref(), Some("notes.notebook.pages"));
    let declaration = param_decl(note, &krate()).to_string();
    assert!(
        declaration.contains("picked_resource_schema")
            && declaration.contains("\"notes.notebook.pages\""),
        "{declaration}"
    );
}

#[test]
fn a_string_parameter_picking_a_resource_declares_its_address() {
    let attr: Attribute = syn::parse_quote!(#[action]);
    let mut method = syn::parse_quote! {
        fn hide(&mut self, #[param("The folder", resource = "files.folder")] folder: String) {}
    };
    let verb = read_verb(&mut method, &attr).expect("picking action parses");
    let folder = &verb.params[0];
    assert_eq!(folder.picks.as_deref(), Some("files.folder"));
    let declaration = param_decl(folder, &krate()).to_string();
    assert!(
        declaration.contains("picked_address_schema")
            && declaration.contains("\"files.folder\"")
            && declaration.contains("ParamType :: Resource"),
        "a string picked from a kind is a resource parameter: {declaration}"
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
    let declaration = param_decl(plain, &krate()).to_string();
    assert!(declaration.contains("const TEXT : & :: core :: primitive :: str = \"Note text\""));
    assert!(declaration.contains("optional : false"));
    assert!(declaration.contains("content : false"));
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

#[test]
fn a_by_app_action_names_no_audience_and_takes_no_prepare() {
    let kept: Attribute = syn::parse_quote!(#[action(housekeeping, by_app)]);
    let options = action_options(&kept).expect("a by_app action parses");
    assert!(options.by_app);
    assert!(options.housekeeping);
    let offered: Attribute = syn::parse_quote!(#[action]);
    assert!(!action_options(&offered).expect("an action parses").by_app);

    let refused: [Attribute; 3] = [
        syn::parse_quote!(#[action(by_app, audience = "agents")]),
        syn::parse_quote!(#[action(audience = "people", by_app)]),
        syn::parse_quote!(#[action(by_app, prepare = fill)]),
    ];
    for attribute in &refused {
        assert!(action_options(attribute).is_err(), "{}", quote!(#attribute));
    }
}
