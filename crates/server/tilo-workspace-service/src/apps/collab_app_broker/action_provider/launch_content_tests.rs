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
