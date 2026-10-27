//! The workspace's emoji in a room whose documents each device's native
//! owner commits: Chat keeps the record, and a Code window on each of two
//! devices reads it and adds to it through its own launch. Both windows and
//! both devices' owners converge on the one record, Chat's.
use super::*;
use tilo_app::framework::WorkspaceRecord;
use tilo_app::tilo_workspace_emoji::{self as emoji, WorkspaceEmoji, workspace_emoji as verbs};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mounted_code_windows_converge_on_the_workspace_emoji() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::WorkspaceEmoji),
    ))
    .await;
}

fn record_id() -> String {
    collab_types::workspace_record_id(emoji::KIND)
}

/// The emoji record as a window holds it, opened where the window opened it.
fn held(app: &Mounted) -> WorkspaceRecord<WorkspaceEmoji> {
    WorkspaceRecord::open(&app.code.workspace, emoji::OWNER, (APP, "Code"), None, None)
        .expect("a Code window opens the workspace's emoji")
}

fn names(record: &WorkspaceRecord<WorkspaceEmoji>) -> Vec<String> {
    record
        .get()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// The emoji `owner` committed to the room's record.
fn committed(owner: &NativeWorkspaceOwner) -> Vec<String> {
    owner
        .with_current(
            RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            |view| {
                Ok(view
                    .peer()
                    .document(emoji::OWNER, &record_id())
                    .and_then(|bytes| {
                        let document = automerge::AutoCommit::load(&bytes).ok()?;
                        <WorkspaceEmoji as tilo_resource::Resource>::read(&document).ok()
                    })
                    .map(|state| state.names().into_iter().map(str::to_owned).collect())
                    .unwrap_or_default())
            },
        )
        .unwrap()
}

/// The emoji record's route as the window holds it: joined, its committed
/// read applied, and no join refused.
fn route_phase(app: &Mounted) -> Option<serde_json::Value> {
    app.code
        .app
        .controlled_document_merge_phase_for_test()
        .into_iter()
        .find(|phase| phase["app"] == emoji::OWNER && phase["record"] == record_id().as_str())
}

fn joined(app: &Mounted) -> bool {
    route_phase(app).is_some_and(|phase| {
        phase["committed_read_ready"] == true && phase["join_failure"].is_null()
    })
}

fn add(record: &WorkspaceRecord<WorkspaceEmoji>, name: &str) {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&[0; 64]);
    record
        .invoke(&WorkspaceEmoji::ADD_EMOJI.with(verbs::AddEmoji {
            name: name.into(),
            image: emoji::EmojiImage(png).to_base64(),
        }))
        .expect("an editor adds an emoji from any app's window");
}

#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn converge(
    context: &Context<'_>,
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
    expected: &[&str],
) {
    let records = mounted.iter().map(held).collect::<Vec<_>>();
    let started = std::time::Instant::now();
    let mut reported = std::time::Instant::now();
    tilo_test_env::within("both Code windows to converge on the workspace's emoji", async {
        loop {
            for app in &mut *mounted {
                app.frame();
            }
            let devices = mounted
                .iter()
                .map(|app| committed(&app.network.native_owner().unwrap()))
                .collect::<Vec<_>>();
            if committed(source) == expected
                && devices.iter().all(|names| names == expected)
                && records.iter().all(|record| names(record) == expected)
                && mounted.iter().all(joined)
            {
                eprintln!("workspace emoji {expected:?} converged after {:?}", started.elapsed());
                break;
            }
            if reported.elapsed() >= std::time::Duration::from_secs(20) {
                eprintln!(
                    "workspace emoji after {:?}: governor {:?}, devices {devices:?}, windows {:?}, routes {:?}",
                    started.elapsed(),
                    committed(source),
                    records.iter().map(names).collect::<Vec<_>>(),
                    mounted.iter().map(route_phase).collect::<Vec<_>>(),
                );
                reported = std::time::Instant::now();
            }
            govern_round(context, authority, source, refreshes).await;
        }
    })
    .await;
}

#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(
    context: &Context<'_>,
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
) {
    // Each window joins Chat's record through its own launch, as Code.
    converge(context, authority, source, refreshes, mounted, &[]).await;
    add(&held(&mounted[0]), "party");
    converge(context, authority, source, refreshes, mounted, &["party"]).await;
    add(&held(&mounted[1]), "ship_it");
    converge(
        context,
        authority,
        source,
        refreshes,
        mounted,
        &["party", "ship_it"],
    )
    .await;
}
