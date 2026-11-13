//! macOS device location through Core Location: one fix, after macOS asks the
//! user, which it does only for a bundled app whose `Info.plist` says why.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_location::{
    CLAuthorizationStatus, CLLocation, CLLocationManager, CLLocationManagerDelegate,
    kCLLocationAccuracyBest,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDate, NSDefaultRunLoopMode, NSError, NSObject, NSObjectProtocol,
    NSRunLoop, NSString,
};

use crate::{LocationChannel, LocationOutcome};

/// How long a request may take, prompt included, as on Linux and the web.
const FIX_TIMEOUT: Duration = Duration::from_secs(20);

const IDLE: Duration = Duration::from_millis(50);

const USAGE_KEY: &str = "NSLocationWhenInUseUsageDescription";

pub(crate) const NO_USAGE_STRING: &str = "Location needs the app's bundle to explain why \
     (NSLocationWhenInUseUsageDescription): run the .app `tilo-app build --target macos` makes.";

pub(crate) struct CoreLocation;

impl crate::LocationProvider for CoreLocation {
    fn locate(&self) -> LocationChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        if !bundle_explains_location() {
            let _ = tx.send(LocationOutcome::Unavailable(NO_USAGE_STRING.to_owned()));
            return rx;
        }
        let wake = tilo_rt::wake::WakeHandle::new();
        let answer = tx.clone();
        let spawned = tilo_rt::spawn_thread("tilo-location", move || {
            let outcome = locate_on_this_thread();
            if answer.send(outcome).is_ok() {
                wake.wake();
            }
        });
        if let Err(error) = spawned {
            let _ = tx.send(LocationOutcome::Unavailable(format!(
                "could not start the location request: {error}"
            )));
        }
        rx
    }
}

pub(crate) fn bundle_explains_location() -> bool {
    let bundle = NSBundle::mainBundle();
    bundle.bundleIdentifier().is_some()
        && bundle
            .objectForInfoDictionaryKey(&NSString::from_str(USAGE_KEY))
            .is_some()
}

#[derive(Default)]
struct Ivars {
    outcome: RefCell<Option<LocationOutcome>>,
    requested: Cell<bool>,
    prompted: Cell<bool>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate`
    // does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = AllocAnyThread]
    #[name = "TiloLocationDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the method signatures match the protocol's.
    unsafe impl CLLocationManagerDelegate for Delegate {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update_locations(
            &self,
            _manager: &CLLocationManager,
            locations: &NSArray<CLLocation>,
        ) {
            let Some(location) = locations.lastObject() else {
                return;
            };
            // SAFETY: plain property reads on a location CoreLocation made.
            let (coordinate, accuracy) =
                unsafe { (location.coordinate(), location.horizontalAccuracy()) };
            self.finish(LocationOutcome::Located {
                lat: coordinate.latitude,
                lon: coordinate.longitude,
                accuracy_m: accuracy.max(0.0),
            });
        }

        #[unsafe(method(locationManager:didFailWithError:))]
        fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
            if error.code() == 0 {
                return;
            }
            let reason = if error.code() == 1 {
                "permission denied".to_owned()
            } else {
                error.localizedDescription().to_string()
            };
            self.finish(LocationOutcome::Unavailable(reason));
        }

        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, manager: &CLLocationManager) {
            self.advance(manager);
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars::default());
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    fn finish(&self, outcome: LocationOutcome) {
        let mut slot = self.ivars().outcome.borrow_mut();
        if slot.is_none() {
            *slot = Some(outcome);
        }
    }

    fn take(&self) -> Option<LocationOutcome> {
        self.ivars().outcome.borrow_mut().take()
    }

    fn advance(&self, manager: &CLLocationManager) {
        let ivars = self.ivars();
        if ivars.requested.get() {
            return;
        }
        // SAFETY: plain calls on a manager made on this thread.
        unsafe {
            match manager.authorizationStatus() {
                CLAuthorizationStatus::NotDetermined => {
                    if !ivars.prompted.replace(true) {
                        manager.requestWhenInUseAuthorization();
                    }
                }
                CLAuthorizationStatus::Denied => {
                    self.finish(LocationOutcome::Unavailable("permission denied".into()));
                }
                CLAuthorizationStatus::Restricted => {
                    self.finish(LocationOutcome::Unavailable(
                        "location is restricted on this Mac".into(),
                    ));
                }
                _ => {
                    ivars.requested.set(true);
                    manager.requestLocation();
                }
            }
        }
    }
}

fn locate_on_this_thread() -> LocationOutcome {
    // SAFETY: a class method with no preconditions.
    if !unsafe { CLLocationManager::locationServicesEnabled_class() } {
        return LocationOutcome::Unavailable("Location Services are off on this Mac".into());
    }
    let delegate = Delegate::new();
    // SAFETY: the manager is made, configured and driven on this thread
    // only, and the delegate outlives it (both drop at the end).
    let manager = unsafe {
        let manager = CLLocationManager::new();
        manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        manager.setDesiredAccuracy(kCLLocationAccuracyBest);
        manager
    };
    delegate.advance(&manager);
    let run_loop = NSRunLoop::currentRunLoop();
    let started = tilo_clock::now();
    let outcome = loop {
        if let Some(outcome) = delegate.take() {
            break outcome;
        }
        if started.elapsed() >= FIX_TIMEOUT {
            break LocationOutcome::Unavailable("timed out waiting for a location fix".into());
        }
        let slice = NSDate::dateWithTimeIntervalSinceNow(0.25);
        // SAFETY: the mode is a static CoreFoundation string; this runs the
        // thread's own run loop.
        let ran = run_loop.runMode_beforeDate(unsafe { NSDefaultRunLoopMode }, &slice);
        // With no source attached (between CoreLocation's callbacks) the run
        // loop returns at once; wait instead of spinning.
        if !ran {
            let _ = tilo_rt::sleep_thread(IDLE);
        }
    };
    // SAFETY: as above; the manager stops before its delegate goes away.
    unsafe {
        manager.stopUpdatingLocation();
        manager.setDelegate(None);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LocationProvider as _;

    #[test]
    fn an_unbundled_process_is_refused_before_corelocation() {
        assert!(!bundle_explains_location());
        let outcome = CoreLocation.locate().recv().unwrap();
        assert_eq!(
            outcome,
            LocationOutcome::Unavailable(NO_USAGE_STRING.to_owned())
        );
        assert!(NO_USAGE_STRING.contains(USAGE_KEY));
    }
}
