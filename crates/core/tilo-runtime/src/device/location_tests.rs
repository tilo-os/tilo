//! What an app's request for this device's location answers.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvocationReceipt, InvokeArgument, InvokeRequest,
    Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};

use super::{LOCATE, Locate, Location, PROVIDER, Position, resource};

const DEVICE: [u8; 32] = [7; 32];
const MAPS: &str = "so.tilo.maps";
const NOTES: &str = "so.tilo.notes";

struct Fixed;

impl Locate for Fixed {
    fn locate(&self) -> Result<Position, String> {
        Ok(Position {
            latitude: 48.1374,
            longitude: 11.5755,
            accuracy_m: 20.0,
        })
    }
}

fn location(declared: &'static [&'static str], switched_off: bool) -> Location {
    Location::new(
        resource(&DEVICE),
        Arc::new(Fixed),
        Arc::new(move |app: &str| declared.contains(&app)),
        Arc::new(move || switched_off),
    )
}

fn request() -> InvokeRequest {
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: ActionAddress::new(resource(&DEVICE), LOCATE),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn answered<T>(pending: Pending<T>) -> T {
    pending
        .wait_timeout(Duration::from_secs(5))
        .ok()
        .flatten()
        .expect("answered")
}

/// An app that declared location is answered where the device is.
#[test]
fn a_granted_app_reads_where_the_device_is() {
    let caller = Caller::local_app(MAPS);
    let InvocationReceipt { status, .. } =
        answered(location(&[MAPS], false).invoke(&caller, request())).expect("answered");
    let ReceiptStatus::Completed { output } = status else {
        panic!("{status:?}");
    };
    assert_eq!(
        output.value,
        Some(json!({"lat": 48.1374, "lon": 11.5755, "accuracy_m": 20.0}))
    );
}

/// An app that did not declare location is asked through the dispatch; that answer is a
/// question, never a fix.
#[test]
fn an_app_without_the_grant_is_asked() {
    let caller = Caller::local_app(NOTES);
    let error = answered(location(&[MAPS], false).invoke(&caller, request()))
        .expect_err("a program without the grant is not answered");
    assert!(
        matches!(error, ActionCallError::ConfirmationRequired(_)),
        "{error:?}"
    );
}

/// The person's lockdown policy turns location off for the person too.
#[test]
fn a_device_with_location_switched_off_refuses() {
    let caller = Caller::local_person("ada");
    let error = answered(location(&[], true).invoke(&caller, request()))
        .expect_err("nothing is answered while location is off");
    assert!(
        matches!(error, ActionCallError::Refused { .. }),
        "{error:?}"
    );
}

/// The person themselves reads their device's location without asking.
#[test]
fn the_person_is_answered() {
    let caller = Caller::local_person("ada");
    let InvocationReceipt { status, .. } =
        answered(location(&[], false).invoke(&caller, request())).expect("answered");
    assert!(
        matches!(status, ReceiptStatus::Completed { .. }),
        "{status:?}"
    );
}
