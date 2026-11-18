//! An app named as a model writes it (`word_processor`, `Word Processor`, `Word`) is the app
//! its id names, and reaches nothing more: an agent held to reading is named the app under
//! every spelling and still changes nothing there, while the same agent of the same person,
//! not held, writes under each of them. Every call goes through the server's own action
//! service and the tools a turn has.
use super::*;

/// The word processor as a model writes it.
const SPELLED: [&str; 7] = [
    "word-processor",
    "word_processor",
    "Word Processor",
    "WordProcessor",
    "Word",
    "so.tilo.word-processor",
    "So.Tilo.Word_Processor",
];
/// What the agent held to reading tries to add under every spelling.
const HELD_TRIES: &str = "Sloe for the warden.";
/// What the same agent adds under every spelling where it is not held.
const FREE_ADDS: &str = "Bullace for the bursar.";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_held_to_reading_changes_nothing_however_it_spells_the_app() {
    let mut served = Served::start(1).await;
    let (workspace, page, _hers) = served.workspace(|_| Vec::new()).await;
    let (cy, _) = tilo_user_identity::UserIdentity::generate("Cy", 1);
    let cy_device = enrol(&cy, "browser", DeviceCapabilities::member());
    admit_as(&served, &workspace, &cy_device, Role::Editor).await;
    let ada = Served::person(&served.ada, &served.devices.1, &workspace);
    let held = from_elsewhere(&cy, &cy_device, &workspace, reading_only(&workspace));
    let free = from_elsewhere(&cy, &cy_device, &workspace, Vec::new());
    let mut wrong: Vec<String> = Vec::new();

    // Held to reading: every spelling names the app to it, and a write there is refused as the
    // write it is. The name was read; nothing was carried out.
    for spelled in SPELLED {
        let listing = serde_json::json!({"path": spelled});
        let (listed, failed) = served.tool(&workspace, &held, "tilo_ls", listing).await;
        if failed || listed.contains("unknown_app") {
            wrong.push(format!("held to reading, {spelled} is not named: {listed}"));
        }
        let add = serde_json::json!({
            "path": format!("{spelled}#append-paragraph"),
            "args": {"text": HELD_TRIES},
        });
        let (added, failed) = served.tool(&workspace, &held, "tilo_do", add).await;
        if !failed || added.contains("unknown_app") {
            wrong.push(format!(
                "held to reading, {spelled}: failed {failed}: {added}"
            ));
        }
    }

    // Not held, the same agent of the same person writes under every spelling.
    let open = serde_json::json!({"path": "word-processor#open", "args": {}});
    let (opened, _) = served.tool(&workspace, &free, "tilo_do", open).await;
    for spelled in SPELLED {
        let add = serde_json::json!({
            "path": format!("{spelled}#append-paragraph"),
            "args": {"text": FREE_ADDS},
        });
        let (added, failed) = served.tool(&workspace, &free, "tilo_do", add).await;
        if failed {
            wrong.push(format!("not held, {spelled}: {added} (open: {opened})"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");

    // What the letter holds afterwards, as its owner finds it.
    assert!(
        finds_a_letter(&served, &workspace, &ada, "Bullace").await,
        "the letter holds what the agent added where it was not held"
    );
    let found = served.finds(&workspace, &ada, "Sloe").await;
    assert!(
        !found.contains(WRITER),
        "nothing the agent held to reading tried stands in the letter: {found}"
    );
    page.peer.close();
}
