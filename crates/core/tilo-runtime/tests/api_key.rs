#![allow(
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::future_not_send,
    reason = "one scenario in order, on a fixture that is not sent between tasks"
)]
//! What a key of the person's reaches through `/v1` besides actions: the one listing an agent's
//! `tilo_ls` prints, read the same from the API; a program's key reaching the host directly, with
//! no API server between them; a file larger than one request going up in pieces that resume;
//! a workspace answering what changed in it, search, and a run taken back whole; and a key's read
//! admitted as the key itself.

use serde_json::{Value, json};

use crate::host;

/// The text of an MCP tool's result, and whether it failed.
fn said(result: &Value) -> (String, bool) {
    let text = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, result["isError"].as_bool().unwrap_or(false))
}

/// One listing, one text: `GET …/resources` answers exactly what the `tilo_ls` tool answers an
/// agent in the same workspace, so a person and an agent read the same listing.
#[test]
fn a_workspaces_listing_reads_the_same_through_mcp_and_the_api() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Notes backend");
        let (agent_read, failed) =
            said(&key.tool("tilo_ls", &json!({"workspace": workspace})).await);
        assert!(!failed, "the agent reads the listing: {agent_read}");
        assert!(!agent_read.is_empty(), "the tool answered lines");

        let (status, listed) = key
            .get(&format!("/v1/workspaces/{workspace}/resources"))
            .await;
        assert_eq!(status, 200, "{listed}");
        let rows = listed["rows"].as_array().cloned().unwrap_or_default();
        assert!(!rows.is_empty(), "the API lists the workspace: {listed}");
        for row in rows {
            let name = row["name"].as_str().unwrap_or_default();
            assert!(
                agent_read.contains(name),
                "the API and the tool list the same rows: {name} in {listed}"
            );
        }
    });
}

/// A program holding a key of the person's own reaches the host directly: the API served in
/// process, the same routes and answers, with no API server between them. A key makes no
/// workspace on a device: the device answers its own, and making one is the computer's change.
#[test]
fn a_key_its_program_holds_reaches_the_host_directly() {
    host::run(|host| async move {
        let key = host.key("Ada's script");

        let (status, info) = key.get("/v1").await;
        assert_eq!(status, 200, "{info}");
        assert_eq!(info["api_version"], "1", "{info}");
        assert_eq!(info["key"]["scope"], "all", "{info}");

        let (status, listed) = key
            .post("/v1/resources/account/workspaces/actions/list", &json!({}))
            .await;
        assert_eq!(status, 200, "{listed}");
        assert!(
            listed["result"]["workspaces"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["id"] == host.workspace())),
            "{listed}"
        );

        let (status, listed) = key
            .get(&format!("/v1/workspaces/{}/resources", host.workspace()))
            .await;
        assert_eq!(status, 200, "{listed}");
        assert!(
            listed["rows"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty()),
            "{listed}"
        );

        // The account's own actions are the computer's: a script's key is told to make a
        // workspace in Tilo itself, where the computer acts.
        let (status, refused) = key
            .post(
                "/v1/resources/account/workspaces/actions/create",
                &json!({"name": "Made from a script"}),
            )
            .await;
        assert_eq!(status, 403, "{refused}");
        assert!(
            refused.to_string().contains("Tilo"),
            "the key is told where a workspace is made: {refused}"
        );
    });
}

/// A file larger than one request goes up in pieces that resume: the first names the file's
/// size, a repeated piece costs nothing, a piece from an offset the put is not at is refused
/// with where it stands, a later piece continues from there, and the completed file is read
/// back byte for byte.
#[test]
fn a_file_larger_than_one_request_goes_up_in_pieces_that_resume() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Visa backend");
        let whole: Vec<u8> = (0..5_000_000_u32).map(|at| (at % 251) as u8).collect();
        let hash = blake3::hash(&whole).to_hex().to_string();
        let base = format!("/v1/workspaces/{workspace}/contents/{hash}");
        let piece = |from: usize, of: usize| whole[from..(from + of).min(whole.len())].to_vec();
        let range = |from: usize, of: usize| {
            let to = (from + of).min(whole.len()) - 1;
            format!("bytes {from}-{to}/{}", whole.len())
        };

        let first = piece(0, 1_000_000);
        let first_range = range(0, 1_000_000);
        let (status, access) = key
            .request(
                "PUT",
                &base,
                &[
                    ("content-type", "application/octet-stream"),
                    ("content-range", first_range.as_str()),
                ],
                &first,
            )
            .await;
        assert_eq!(status, 200, "{access}");
        assert_eq!(access["offset"], json!(1_000_000), "{access}");
        assert_eq!(access["size"], json!(whole.len()), "{access}");

        let (status, again) = key
            .request(
                "PUT",
                &base,
                &[
                    ("content-type", "application/octet-stream"),
                    ("content-range", first_range.as_str()),
                ],
                &first,
            )
            .await;
        assert_eq!(
            (status, &again["offset"]),
            (200, &json!(1_000_000)),
            "{again}"
        );

        let ahead = range(2_000_000, 1_000_000);
        let (status, refused) = key
            .request(
                "PUT",
                &base,
                &[
                    ("content-type", "application/octet-stream"),
                    ("content-range", ahead.as_str()),
                ],
                &piece(2_000_000, 1_000_000),
            )
            .await;
        assert_eq!(
            (status, &refused["error"]["code"]),
            (409, &json!("offset_mismatch")),
            "{refused}"
        );

        let mut at = 1_000_000;
        let mut content = Value::Null;
        while at < whole.len() {
            let chunk = piece(at, 1_000_000);
            let chunk_range = range(at, chunk.len());
            let (status, answered) = key
                .request(
                    "PUT",
                    &base,
                    &[
                        ("content-type", "application/octet-stream"),
                        ("content-range", chunk_range.as_str()),
                    ],
                    &chunk,
                )
                .await;
            assert!(status == 200 || status == 201, "{answered}");
            at += chunk.len();
            assert_eq!(
                answered["content"].is_string(),
                at == whole.len(),
                "{answered}"
            );
            if at == whole.len() {
                content = answered["content"].clone();
            }
        }
        let content = content
            .as_str()
            .expect("the put names its content")
            .to_owned();
        let reference = content
            .strip_prefix("tilo.content:")
            .expect("a content reference");
        let (status, body) = key
            .bytes(&format!("/v1/workspaces/{workspace}/contents/{reference}"))
            .await;
        assert_eq!(status, 200);
        assert_eq!(body, whole, "every byte of the file, byte for byte");
    });
}

/// What a workspace answers a key besides actions: what changed in it, as it changes; search
/// over what it keeps; and a run taken back whole.
#[test]
fn a_workspace_streams_changes_finds_and_undoes_runs() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let key = host.key("Notes backend");
        let base = format!("/v1/workspaces/{workspace}");
        let stream = key.stream("GET", &format!("{base}/events"), &[], b"").await;

        // A run of the person's, changing a record that search then finds.
        let (status, invited) = key
            .request(
                "POST",
                &format!("{base}/resources/workspace/invitations/actions/invite"),
                &[
                    ("content-type", "application/json"),
                    ("tilo-invocation", "invite"),
                    ("tilo-invocation-title", "Invite the reviewer"),
                ],
                json!({"role": "editor", "invitee_name": "Gooseberry jam for the embassy"})
                    .to_string()
                    .as_bytes(),
            )
            .await;
        assert_eq!(status, 200, "{invited}");
        assert_eq!(invited["status"], "ok", "{invited}");
        let run = invited["invocation"].as_str().map(str::to_owned);

        // Search finds what the workspace keeps.
        let (status, found) = key
            .post(
                &format!("{base}/resources/search/actions/find"),
                &json!({"query": "Gooseberry"}),
            )
            .await;
        assert_eq!(status, 200, "the workspace answers search: {found}");
        assert!(
            found["result"]["hits"].to_string().contains("Gooseberry"),
            "{found}"
        );

        // The run is taken back whole, by its id.
        let run = run.expect("the run's id");
        let (status, undone) = key
            .post(&format!("{base}/invocations/{run}/undo"), &json!({}))
            .await;
        assert_eq!((status, &undone["status"]), (200, &json!("ok")), "{undone}");

        // Record changes come down the stream.
        let within = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut seen = String::new();
        while !seen.contains("event: change") {
            let chunk = tokio::time::timeout_at(within, stream.next())
                .await
                .unwrap_or_else(|_| panic!("a change comes down the stream within 30 s: {seen}"))
                .expect("the stream stays open");
            seen.push_str(&String::from_utf8_lossy(&chunk));
        }
    });
}

/// A key's read is the key's: it is admitted as the caller its certificate names, not as the
/// person whose computer serves the workspace, so the workspace's grants decide it for the key's
/// own device.
#[test]
fn a_keys_read_is_admitted_as_the_key() {
    host::run(|host| async move {
        let page = host.page_key("A page");
        let (status, listed) = page
            .get(&format!("/v1/workspaces/{}/resources", host.workspace()))
            .await;
        assert_eq!(status, 200, "a key's read is admitted as the key: {listed}");
    });
}
