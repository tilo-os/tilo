//! Typed bounded jobs on an existing acknowledged record channel.
use runtime_collab::approved_checks::{Receipt, Request};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tokio::sync::{mpsc, oneshot};

const MAX_ROUTES: usize = 16;
const MAX_PENDING: usize = 2;
type Reply = Result<Receipt, String>;
struct Job {
    request: Request,
    reply: oneshot::Sender<Reply>,
}
struct Endpoint(mpsc::Sender<Job>);
pub(super) struct Registration {
    _endpoint: Arc<Endpoint>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

fn registry() -> &'static Mutex<BTreeMap<String, Weak<Endpoint>>> {
    static REGISTRY: OnceLock<Mutex<BTreeMap<String, Weak<Endpoint>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

pub(crate) fn enqueue(route: &str, request: Request) -> Result<oneshot::Receiver<Reply>, String> {
    request.encode()?;
    let endpoint = registry()
        .lock()
        .map_err(|_| "approved-check bridge unavailable")?
        .get(route)
        .and_then(Weak::upgrade)
        .ok_or("no acknowledged approved-check route")?;
    let (reply, receive) = oneshot::channel();
    endpoint
        .0
        .try_send(Job { request, reply })
        .map_err(|_| "approved-check bounded lane unavailable or full")?;
    Ok(receive)
}

fn admit(route: String, endpoint: &Arc<Endpoint>) -> Result<(), String> {
    let mut entries = registry()
        .lock()
        .map_err(|_| "approved-check bridge unavailable")?;
    entries.retain(|_, endpoint| endpoint.strong_count() != 0);
    if route.is_empty()
        || route.len() > 1024
        || entries.contains_key(&route)
        || entries.len() >= MAX_ROUTES
    {
        return Err("approved-check route is ambiguous or exceeds admission bounds".into());
    }
    entries.insert(route, Arc::downgrade(endpoint));
    Ok(())
}

/// Register only the capability returned by the existing authenticated join.
/// Duplicate live routes refuse ambiguity rather than selecting another instance.
pub(super) fn register(
    route: String,
    channel: runtime_collab::ChannelHandle,
) -> Result<Registration, String> {
    let (send, mut receive) = mpsc::channel::<Job>(MAX_PENDING);
    let endpoint = Arc::new(Endpoint(send));
    admit(route, &endpoint)?;
    let worker = tokio::task::spawn_local(async move {
        while let Some(job) = receive.recv().await {
            if job.reply.is_closed() {
                continue;
            }
            let result = channel
                .approved_check(&job.request)
                .await
                .map_err(|error| error.to_string());
            let _ = job.reply.send(result);
        }
    });
    Ok(Registration {
        _endpoint: endpoint,
        worker,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_check_bridge_routes_two_live_records_of_one_app_by_actual_dispatch_target() {
        let room = uuid::Uuid::new_v4();
        let first_route =
            crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "first-record");
        let second_route =
            crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "second-record");
        let (first_send, mut first_receive) = mpsc::channel(MAX_PENDING);
        let (second_send, mut second_receive) = mpsc::channel(MAX_PENDING);
        let first = Arc::new(Endpoint(first_send));
        let second = Arc::new(Endpoint(second_send));
        admit(first_route.channel_id(), &first).unwrap();
        admit(second_route.channel_id(), &second).unwrap();
        let address = tilo_actions::ActionAddress::new(
            "repository",
            runtime_collab::approved_checks::ACTION_ID,
        );
        let first_target =
            crate::host_operation::OperationTarget::new(first_route, address.clone());
        let second_target =
            crate::host_operation::OperationTarget::new(second_route, address.clone());
        let first_id = tilo_actions::InvocationId::new();
        let second_id = tilo_actions::InvocationId::new();
        let first_reply = first_target.approved_check(first_id, "regression").unwrap();
        assert_eq!(
            first_receive.try_recv().unwrap().request.invocation,
            first_id.0.to_string()
        );
        assert!(
            second_receive.try_recv().is_err(),
            "same app on a second record receives no request"
        );
        let second_reply = second_target
            .approved_check(second_id, "regression")
            .unwrap();
        assert_eq!(
            second_receive.try_recv().unwrap().request.invocation,
            second_id.0.to_string()
        );
        assert!(first_receive.try_recv().is_err());
        let foreign = crate::host_operation::OperationTarget::new(
            crate::collaboration::RouteSpec::record("so.tilo.git-client", room, "foreign-record"),
            address,
        );
        assert!(foreign.approved_check(first_id, "regression").is_err());
        drop(first_reply);
        drop(second_reply);
        drop(first);
        assert!(first_target.approved_check(first_id, "regression").is_err());
    }

    #[test]
    fn approved_check_bridge_refuses_ambiguous_foreign_full_and_retired_routes() {
        let route = format!("so.tilo.git#{}#record:project", uuid::Uuid::new_v4());
        let (send, mut receive) = mpsc::channel(MAX_PENDING);
        let endpoint = Arc::new(Endpoint(send));
        admit(route.clone(), &endpoint).unwrap();
        let request = Request {
            version: 1,
            invocation: uuid::Uuid::new_v4().to_string(),
            check_id: "regression".into(),
        };
        let (other_send, _) = mpsc::channel(MAX_PENDING);
        let other = Arc::new(Endpoint(other_send));
        assert!(
            admit(route.clone(), &other).is_err(),
            "never select a second live endpoint for the same record"
        );
        assert!(enqueue(&format!("{route}-foreign"), request.clone()).is_err());
        let first = enqueue(&route, request.clone()).unwrap();
        let second = enqueue(&route, request.clone()).unwrap();
        assert!(
            enqueue(&route, request.clone()).is_err(),
            "actual typed lane refuses a third queued job"
        );
        assert_eq!(receive.try_recv().unwrap().request, request);
        assert!(
            enqueue(
                &route,
                Request {
                    check_id: "../raw-path".into(),
                    ..request.clone()
                }
            )
            .is_err()
        );
        drop(first);
        drop(second);
        drop(receive);
        assert!(
            enqueue(&route, request.clone()).is_err(),
            "closed receiver is not silently successful"
        );
        drop(endpoint);
        assert!(
            enqueue(&route, request).is_err(),
            "a weak registry entry cannot retain retired transport"
        );
    }
}
