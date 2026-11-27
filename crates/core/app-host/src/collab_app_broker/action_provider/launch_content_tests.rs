use super::launch_content_hash;

#[test]
fn an_empty_typed_argument_map_opens_without_content() {
    let empty = format!("{}{{}}", tilo_actions::ARGUMENTS_JSON_PREFIX);
    assert_eq!(launch_content_hash(Some(&empty)), "");
    assert_eq!(launch_content_hash(None), "");
    assert_eq!(launch_content_hash(Some("sha256:abc")), "sha256:abc");
    let nonempty = format!(
        "{}{{\"content\":\"x\"}}",
        tilo_actions::ARGUMENTS_JSON_PREFIX
    );
    assert_eq!(launch_content_hash(Some(&nonempty)), nonempty);
}

#[test]
fn a_file_handed_over_by_its_name_opens_its_content() {
    let hash = "ab".repeat(32);
    let tilo_actions::remote::InvokeArgument::Json(map) =
        collab_types::app_actions::ApplicationLaunch::with_content("so.tilo.notes", &hash)
            .named("plan.txt")
            .argument()
    else {
        panic!("a named launch sends its argument map");
    };
    let named = format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::Value::Object(map)
    );
    assert_eq!(launch_content_hash(Some(&named)), hash);
}
