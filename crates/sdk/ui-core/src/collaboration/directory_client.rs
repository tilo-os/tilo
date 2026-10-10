//! Client adapters use the existing bounded collaboration lane. They carry
//! inert directory inputs; only the real broker admits an invocation.
use super::{ByteMeter, Duration, SdkRoute, serialized_bytes};

const DIRECTORY_RPC_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a resolved invocation waits for the broker's admission answer.
/// Admission journals the invocation durably, which takes seconds on a loaded
/// host, and a missed answer can only be reported as an unknown outcome. This
/// bounds a hung broker, not an ordinary slow one.
const ADMISSION_RPC_TIMEOUT: Duration = Duration::from_secs(60);

fn admission_timeout() -> Duration {
    #[cfg(test)]
    if let Some(timeout) = tests_admission_timeout() {
        return timeout;
    }
    ADMISSION_RPC_TIMEOUT
}

/// A test bound for admission answers, so a never-answered invocation's
/// unknown outcome is observed without waiting out the production bound.
#[cfg(test)]
static TEST_ADMISSION_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
fn tests_admission_timeout() -> Option<Duration> {
    match TEST_ADMISSION_TIMEOUT_MS.load(std::sync::atomic::Ordering::SeqCst) {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    }
}

#[cfg(test)]
pub(super) fn set_test_admission_timeout(timeout: Duration) {
    TEST_ADMISSION_TIMEOUT_MS.store(
        u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        std::sync::atomic::Ordering::SeqCst,
    );
}

pub(super) fn query_bytes(query: &runtime_collab::ActionDirectoryQuery, meter: &mut ByteMeter) {
    meter.string_capacity(&query.revision);
    meter.bytes(&query.cursor);
    meter.string_capacity(&query.needle);
    meter.string_capacity(&query.parent_resource);
}

fn row_bytes(row: &runtime_collab::ActionDirectoryRow, meter: &mut ByteMeter) {
    for field in [
        &row.parent_resource,
        &row.resource,
        &row.resource_kind,
        &row.resource_label,
    ] {
        meter.string_capacity(field);
    }
    if let Some(presentation) = &row.presentation {
        serialized_bytes(presentation, meter);
    }
    if let Some(action) = &row.action {
        meter.string_capacity(action);
    }
    if let Some(visibility) = &row.visibility {
        meter.string_capacity(&visibility.action);
        serialized_bytes(&visibility.contract, meter);
    }
    if let Some(action) = &row.descriptor {
        for field in [
            &action.resource,
            &action.action,
            &action.resource_kind,
            &action.resource_label,
            &action.action_label,
        ] {
            meter.string_capacity(field);
        }
        serialized_bytes(&action.params, meter);
        serialized_bytes(&action.contract, meter);
        if let Some(view) = &action.view {
            serialized_bytes(view, meter);
        }
        if let Some(description) = &action.description {
            meter.string_capacity(description);
        }
        if let runtime_collab::ExportedInput::Text {
            prompt,
            placeholder,
            ..
        } = &action.input
        {
            meter.string_capacity(prompt);
            meter.string_capacity(placeholder);
        }
    }
}

pub(super) fn page_bytes(page: &runtime_collab::ActionDirectoryPage, meter: &mut ByteMeter) {
    meter.string_capacity(&page.revision);
    meter.bytes(&page.next_cursor);
    meter.add_product(
        page.rows.capacity(),
        std::mem::size_of::<runtime_collab::ActionDirectoryRow>(),
    );
    for row in &page.rows {
        row_bytes(row, meter);
    }
}

pub(super) fn resolution_bytes(
    resolution: &runtime_collab::ActionDirectoryResolution,
    meter: &mut ByteMeter,
) {
    if let runtime_collab::ActionDirectoryResolution::Found { row, revision } = resolution {
        meter.add(std::mem::size_of::<runtime_collab::ActionDirectoryRow>());
        row_bytes(row, meter);
        if let Some(revision) = revision {
            meter.string_capacity(revision);
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
pub(super) async fn query(
    route: &SdkRoute,
    query: &runtime_collab::ActionDirectoryQuery,
) -> Result<runtime_collab::ActionDirectoryPage, String> {
    tokio::time::timeout(
        DIRECTORY_RPC_TIMEOUT,
        route.channel.query_action_directory(query),
    )
    .await
    .map_err(|_| "action directory query timed out".to_string())?
    .map_err(|error| error.to_string())
}
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
pub(super) async fn resolve(
    route: &SdkRoute,
    peer_id: u64,
    revision: &str,
    resource: &str,
    action: &str,
) -> Result<runtime_collab::ActionDirectoryResolution, String> {
    tokio::time::timeout(
        DIRECTORY_RPC_TIMEOUT,
        route
            .channel
            .resolve_published_action(peer_id, revision, resource, action),
    )
    .await
    .map_err(|_| "action directory resolution timed out".to_string())?
    .map_err(|error| error.to_string())
}
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc channel futures are !Send by design; driven only on the shovel LocalSet"
)]
pub(super) async fn invoke(
    route: &SdkRoute,
    peer_id: u64,
    revision: &str,
    resource: &str,
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
    confirmation: serde_json::Value,
) -> Result<(u64, tilo_actions::InvocationUpdate), String> {
    let argument = format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::Value::Object(arguments)
    );
    // An unsupported revision-bound method must remain unavailable. Never
    // retry the eager endpoint, which cannot enforce the selected revision.
    // Losing the response is not proof that admission or an effect failed.
    // This wire method returns its server-generated identity only in the
    // response; timeout cannot safely retry or invent a terminal receipt.
    tokio::time::timeout(admission_timeout(), route.channel.invoke_resolved_published_action(
            peer_id,
            revision,
            resource,
            action,
            Some(&argument),
            &confirmation.to_string(),
        ))
        .await
        .map_err(|_| "resolved_invocation_outcome_unknown: admission response timed out; the action may have been admitted or executed; do not retry".to_string())?
        .map_err(|error| error.to_string())
        .and_then(|(id, update)| {
            serde_json::from_str(&update)
                .map(|update| (id, update))
                .map_err(|error| error.to_string())
        })
}
