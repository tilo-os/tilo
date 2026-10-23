#[test]
fn native_fixture() {
    if std::env::var("TILO_IMAGER_FRONTEND_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .unwrap();
    let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let response = match request["operation"].as_str().unwrap() {
        "selected-grant" => super::qualification_selected_grant::fixture(&request),
        "paint-flatpak" => {
            let home = std::path::Path::new(request["home"].as_str().unwrap());
            // Rediscover from the same actual user's installation; do not pass
            // synthetic inventory into the UI just to satisfy the native gate.
            let inventory = super::flatpak_transfer::discover(&super::flatpak_transfer::NativeRunner, home).unwrap();
            super::journey::paint_native_flatpak_inventory(home, &inventory);
            serde_json::Value::Null
        }
        "prepare-story" => {
            let home = std::path::Path::new(request["home"].as_str().unwrap());
            let destination = std::path::Path::new(request["destination"].as_str().unwrap());
            let password = request["password"].as_str().unwrap();
            let preparation = if let Some(endpoint) = request["endpoint"].as_str() {
                super::journey::main_story_tests::prepare_native_portable_story_with_dav(home, destination, password, &reqwest::Url::parse(endpoint).unwrap())
            } else {
                super::journey::main_story_tests::prepare_native_portable_story(home, destination, password)
            };
            serde_json::to_value(preparation).unwrap()
        }
        "bundled-releases" => serde_json::Value::Array(crate::release::bundled_releases().unwrap().into_iter().map(|release| {
            serde_json::json!({"size": release.size, "image_sha256": release.image_sha256})
        }).collect()),
        operation => panic!("unknown frontend fixture operation: {operation}"),
    };
    println!(
        "TILO_FRONTEND_RESPONSE {}",
        serde_json::to_string(&response).unwrap()
    );
}
