//! An action's name belongs to its resource type: `join` on a Wi-Fi network and `join` on a saved
//! network are two actions, each with its own id, in a package's own declarations and in one it
//! uses. A call on a resource (`n.join()`) is the action of that resource's type, and a call that
//! names the resource first (`join(n)`) chooses by it.

use crate::common::{codes, codes_with, compile_with, validate};
use tilolang::{Compiled, PackageMeta};

const NETWORKS: &str = "type Wifi {\n    name: Text\n    joined: Bool = false\n}\ntype Saved {\n    name: Text\n    auto: Bool = false\n}\ntype Radio {\n    networks: Collection<Wifi>\n    saved: Collection<Saved>\n}\nroot Radio\ndefault view radios for Radio\nview radios(r: Radio) {\n    text(\"Networks\")\n}\n/// Join the network.\naction join(n: Wifi) {\n    n.joined = true\n}\n/// Join the saved network.\naction join(s: Saved) {\n    s.auto = true\n}\n/// Set whether it joins by itself.\naction set_auto(s: Saved, on: Bool) {\n    s.auto = on\n}\n";

fn compiled(app: &str, body: &str, imports: &[PackageMeta]) -> Compiled {
    let src = format!("app {app} \"Networks\"\n{body}\n");
    let compiled = compile_with(&src, imports).unwrap_or_else(|d| panic!("{body}\n{d:#?}"));
    validate(&compiled.wasm);
    compiled
}

fn networks(view: &str) -> Compiled {
    compiled("t.net", &format!("{NETWORKS}{view}"), &[])
}

/// The id of the package's action `name` whose first parameter is of type `on`.
fn id_of(meta: &PackageMeta, name: &str, on: &str) -> String {
    meta.actions
        .iter()
        .find(|action| action.name == name && action.params.first().is_some_and(|p| p.ty == on))
        .unwrap_or_else(|| panic!("no {name} on {on}: {:#?}", meta.actions))
        .id
        .clone()
}

/// The ids of the actions the package's view `view` applies.
fn reaches(meta: &PackageMeta, view: &str) -> Vec<String> {
    let view = meta
        .views
        .iter()
        .find(|v| v.name == view)
        .expect("the view");
    let mut ids: Vec<String> = view.reaches.iter().map(|r| r.action.clone()).collect();
    ids.sort();
    ids
}

fn sorted<const N: usize>(ids: [&String; N]) -> Vec<String> {
    let mut ids: Vec<String> = ids.into_iter().cloned().collect();
    ids.sort();
    ids
}

tilo_test_env::scenario! {
    /// Two resource types each have their own action of one name, and a call reaches the one of
    /// the resource it is made on.
    fn an_actions_name_belongs_to_its_resource_type;

    phase two_types_each_declare_their_own_join;
    {
        let net = networks("view radio(r: Radio) {\n    text(\"x\")\n}");
        let wifi = id_of(&net.meta, "join", "Wifi");
        let saved = id_of(&net.meta, "join", "Saved");
        assert_ne!(wifi, saved, "each its own id");
        // Each type's join keeps its id under its own name in the lock, so a build gives both
        // the ids the last one did.
        assert_eq!(net.ids.get("join@Wifi"), Some(wifi.as_str()), "{:?}", net.ids);
        assert_eq!(net.ids.get("join@Saved"), Some(saved.as_str()), "{:?}", net.ids);
    }

    phase a_call_on_a_resource_is_its_types_action;
    {
        let net = networks(
            "view wifi(r: Radio) {\n    for n in r.networks {\n        ui.Button(n.name) {\n            on press: n.join()\n        }\n    }\n}\nview saved(r: Radio) {\n    for s in r.saved {\n        ui.Button(s.name) {\n            on press: s.join()\n        }\n        ui.Button(\"Auto\") {\n            on press: s.set_auto(not s.auto)\n        }\n    }\n}\nview named(r: Radio) {\n    for s in r.saved {\n        ui.Button(s.name) {\n            on press: join(s)\n        }\n    }\n}",
        );
        let wifi = id_of(&net.meta, "join", "Wifi");
        let saved = id_of(&net.meta, "join", "Saved");
        let auto = id_of(&net.meta, "set_auto", "Saved");
        assert_eq!(reaches(&net.meta, "wifi"), [wifi]);
        assert_eq!(reaches(&net.meta, "saved"), sorted([&saved, &auto]));
        assert_eq!(reaches(&net.meta, "named"), [saved]);
    }

    phase a_call_names_the_resource_it_chooses_by;
    {
        // Without a resource first, the call cannot say whose join it is.
        let got = codes(&format!(
            "{NETWORKS}view radio(r: Radio) {{\n    ui.Button(\"Join\") {{\n        on press: join(\"Home\")\n    }}\n}}"
        ));
        assert!(got.iter().any(|c| c == "E022"), "{got:?}");
        // A type with no action of the name has none to call.
        let got = codes(&format!(
            "{NETWORKS}view radio(r: Radio) {{\n    ui.Button(\"Join\") {{\n        on press: join(r)\n    }}\n}}"
        ));
        assert!(got.iter().any(|c| c == "E022"), "{got:?}");
    }

    phase one_type_has_one_action_of_a_name;
    {
        let got = codes(&format!(
            "{NETWORKS}action join(n: Wifi, quietly: Bool) {{\n    n.joined = quietly\n}}\nview radio(r: Radio) {{\n    text(\"x\")\n}}"
        ));
        assert!(got.iter().any(|c| c == "E011"), "{got:?}");
        // Nor does a fn share an action's name.
        let got = codes(&format!(
            "{NETWORKS}fn join(t: Text) -> Text {{\n    return t\n}}\nview radio(r: Radio) {{\n    text(\"x\")\n}}"
        ));
        assert!(got.iter().any(|c| c == "E011"), "{got:?}");
    }

    phase another_packages_types_keep_their_own_actions_too;
    {
        let net = networks("view radio(r: Radio) {\n    text(\"x\")\n}");
        let wifi = id_of(&net.meta, "join", "Wifi");
        let saved = id_of(&net.meta, "join", "Saved");
        let imports = std::slice::from_ref(&net.meta);
        let user = compiled(
            "t.y",
            "use t.net as net\n\nview on_row(n: net.Wifi) {\n    ui.Button(\"Join\") {\n        on press: n.join()\n    }\n}\nview by_name(s: net.Saved) {\n    ui.Button(\"Join\") {\n        on press: net.join(s)\n    }\n}",
            imports,
        );
        assert_eq!(reaches(&user.meta, "on_row"), [wifi]);
        assert_eq!(reaches(&user.meta, "by_name"), [saved]);
        // A resource of a type with no such action is refused.
        let got = codes_with(
            "use t.net as net\n\nview pick(r: net.Radio) {\n    ui.Button(\"Join\") {\n        on press: net.join(r)\n    }\n}",
            imports,
        );
        assert!(got.iter().any(|c| c == "E022"), "{got:?}");
    }
}

const SAVED_JOIN: &str =
    "/// Join the saved network.\naction join(s: Saved) {\n    s.auto = true\n}\n";

/// The networks package with the ids `lock` gave its last build.
fn networks_after(body: &str, lock: &tilolang::IdsLock, seed: u64) -> Compiled {
    let src = format!("app t.net \"Networks\"\n{body}\n");
    tilolang::compile_package_with(&src, lock, &[], &mut tilolang::SeededEntropy(seed))
        .unwrap_or_else(|d| panic!("{body}\n{d:#?}"))
}

tilo_test_env::scenario! {
    /// An action's id is its own: a second resource type declaring the same verb leaves the
    /// first type's action, and what was stored or granted under its id, as it was.
    fn an_actions_id_stays_when_another_type_declares_its_verb;

    phase a_second_types_join_leaves_the_first_ones_id;
    {
        let one = NETWORKS.replace(SAVED_JOIN, "");
        assert_ne!(one, NETWORKS, "the package starts with one join");
        let first = networks_after(&one, &tilolang::IdsLock::new(), 1);
        let wifi = id_of(&first.meta, "join", "Wifi");
        let second = networks_after(NETWORKS, &first.ids, 2);
        assert_eq!(id_of(&second.meta, "join", "Wifi"), wifi, "{:?}", second.ids);
        assert_ne!(id_of(&second.meta, "join", "Saved"), wifi);
        assert!(
            second.ids.retired().next().is_none(),
            "nothing the first build named is retired: {:?}",
            second.ids
        );
        // Taking the second join away again leaves the first one's id too.
        let third = networks_after(&one, &second.ids, 3);
        assert_eq!(id_of(&third.meta, "join", "Wifi"), wifi);
    }
}
