//! One location fix from `LocationManager`, after the runtime permission.

use super::bridge;
use crate::host::LocationProvider;
use crate::location::{LocationChannel, LocationOutcome};
use crate::wake::reply;

const FINE: &str = "android.permission.ACCESS_FINE_LOCATION";
const COARSE: &str = "android.permission.ACCESS_COARSE_LOCATION";

pub(super) struct SystemLocation;

impl LocationProvider for SystemLocation {
    fn locate(&self) -> LocationChannel {
        let (tx, rx) = reply();
        let granted = [COARSE, FINE]
            .iter()
            .any(|permission| bridge::is_granted(permission).unwrap_or(false));
        if granted {
            bridge::locate(tx);
            return rx;
        }
        bridge::request_permissions(&[FINE, COARSE], move |answer| match answer {
            Ok(true) => bridge::locate(tx),
            Ok(false) => {
                tx.send(LocationOutcome::Unavailable(
                    "the user did not allow location".to_string(),
                ));
            }
            Err(error) => {
                tx.send(LocationOutcome::Unavailable(error));
            }
        });
        rx
    }
}
