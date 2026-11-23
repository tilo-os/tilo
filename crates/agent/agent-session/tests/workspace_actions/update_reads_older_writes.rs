//! A server updated from a build that named each app's surface by the hash of its `surface.toml`
//! still lists, over the API, the issues that build wrote. Every change names the
//! surface of the app version that made it, and the server keeps each surface its workspaces'
//! changes name in its content store, in the form it was named by: an earlier build kept the
//! `surface.toml` itself there, and the changes it wrote name the hash of those bytes. The update
//! reads them as they are.

use super::api_on_server::{api_on_server, approved_key, headers, json_of};
use super::*;
use collab_types::surface::{KindSurface, SurfaceFile};
use serde_json::{Value, json};

const ISSUES: usize = 12;
/// How long after the restart a listing may take to answer with what was written before.
const BOUND: Duration = Duration::from_secs(60);

/// The app the earlier build wrote with, and the `surface.toml` it carried for it.
const EARLIER: [(&str, &str); 1] = [(
    "so.tilo.tasks",
    include_str!("../../../../../apps/tasks/surface.toml"),
)];

/// How the earlier build decided changes: by the surface a change names, read from the content
/// store `dir` as the `surface.toml` it kept there, or the one the build carries for the app.
struct EarlierBuild {
    dir: std::path::PathBuf,
}

impl collab_types::SurfaceSource for EarlierBuild {
    fn surface(
        &self,
        document: &collab_types::WrittenDocument<'_>,
        kind: Option<&str>,
        named: Option<&[u8; 32]>,
    ) -> Option<Arc<dyn collab_types::ReceiptSurface>> {
        let named = named?;
        let file = match std::fs::read(self.dir.join(hex::encode(named))) {
            Ok(bytes) if blake3::hash(&bytes).as_bytes() == named => {
                SurfaceFile::parse(&bytes).ok()?
            }
            _ => {
                let carried = collab_types::app_surface::bytes_of(document.app)?;
                if blake3::hash(carried).as_bytes() != named {
                    return None;
                }
                SurfaceFile::from_json(std::str::from_utf8(carried).ok()?).ok()?
            }
        };
        let resource = match kind {
            Some(kind) => file.resource(kind)?,
            None => file.own_resource()?,
        };
        Some(Arc::new(KindSurface(resource.clone())))
    }
}

async fn call(
    api: &Arc<tilo_api::Server>,
    token: &str,
    method: hyper::Method,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let bearer = format!("Bearer {token}");
    let headers = headers(&[
        ("authorization", bearer.as_str()),
        ("content-type", "application/json"),
    ]);
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let body = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    json_of(Box::pin(api.dispatch(&method, path, query, &headers, &body)).await).await
}

#[test]
fn an_update_lists_what_a_build_that_named_surfaces_by_their_file_wrote() {
    // The surfaces a process decides and signs changes with are the process's: the update
    // replaces them.
    tilo_test_env::in_own_process(
        module_path!(),
        "an_update_lists_what_a_build_that_named_surfaces_by_their_file_wrote",
        || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(written_before_listed_after());
        },
    );
}

async fn written_before_listed_after() {
    // The earlier build: it kept each app's `surface.toml` in the server's content store, and
    // named the hash of those bytes in every change it signed.
    let store = tempfile::tempdir().unwrap();
    for (_, file) in EARLIER {
        std::fs::write(
            store
                .path()
                .join(blake3::hash(file.as_bytes()).to_hex().as_str()),
            file,
        )
        .unwrap();
    }
    tilo_peer_state::decide_with_surfaces(
        collab_types::Surfaces::from_source(Arc::new(EarlierBuild {
            dir: store.path().to_path_buf(),
        })),
        |app| {
            EARLIER
                .iter()
                .find(|(earlier, _)| *earlier == app)
                .map(|(_, file)| *blake3::hash(file.as_bytes()).as_bytes())
                .or_else(|| collab_types::app_surface::hash_of(app))
        },
    );
    let on_server = api_on_server(super::api::ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let browser = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;
    let (status, workspace) = call(
        &on_server.api,
        &token,
        hyper::Method::POST,
        "/v1/resources/account/workspaces/actions/create",
        &json!({"name": "Team"}),
    )
    .await;
    assert_eq!(status, 200, "{workspace}");
    let base = format!(
        "/v1/workspaces/{}",
        workspace["result"]["workspace"]["id"].as_str().unwrap()
    );
    let writes = [
        ("tasks", "open", json!({})),
        (
            "tasks/tasks.issues",
            "create-many",
            json!({"issues": (0..ISSUES)
                .map(|at| json!({"title": format!("Issue {at}"), "status": "todo"}))
                .collect::<Vec<_>>()}),
        ),
    ];
    for (resource, action, args) in writes {
        let (status, reply) = call(
            &on_server.api,
            &token,
            hyper::Method::POST,
            &format!("{base}/resources/{resource}/actions/{action}"),
            &args,
        )
        .await;
        assert_eq!(status, 200, "{resource}#{action}: {reply}");
    }

    // The update: this build's start decides changes against the content store as it reads it,
    // and the server starts again on the state the earlier one kept.
    tilo_server::surfaces::decide_receipt(store.path());
    let on_server = on_server.restart(false).await;
    let token = approved_key(&on_server, &ada, &browser, "Mover").await;

    let started = Instant::now();
    let mut last = Value::Null;
    'listed: for (path, rows) in [("tasks/tasks.issues", ISSUES)] {
        while started.elapsed() < BOUND {
            let (status, page) = call(
                &on_server.api,
                &token,
                hyper::Method::GET,
                &format!("{base}/resources?path={path}&limit=200"),
                &Value::Null,
            )
            .await;
            let listed = page["rows"].as_array().map_or(0, Vec::len);
            if status == 200 && listed >= rows {
                println!(
                    "the updated server listed {path} as the earlier build wrote it {:?} after its start",
                    started.elapsed()
                );
                continue 'listed;
            }
            last = json!({"path": path, "status": status, "rows": listed, "page": page});
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        panic!(
            "what the earlier build wrote was not listed within {BOUND:?} of the update: {last}"
        );
    }
}
