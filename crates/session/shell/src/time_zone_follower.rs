//! The time zone that follows the device.
//!
//! Settings' time-zone picker offers "Set automatically" first; choosing it
//! writes [`settings_store::clock::AUTOMATIC_ZONE`]. While it is on, this
//! session asks for the device's location (the XDG Location portal, which
//! asks `GeoClue`) when it starts, when the choice is turned on and every
//! [`LOOK_AGAIN`] after, and sets the zone of the installed tzdata's city
//! nearest to where the device is, through timedated. A zone the person
//! picks turns the choice off, and this leaves the zone alone from then on.
//!
//! Nothing is guessed: without a location (no `GeoClue`, permission denied,
//! no fix) the zone stays as it is.

use std::time::Duration;

use settings_store::day_schedule::Location;

/// How often a device whose zone follows it looks where it is again: often
/// enough that a flight's arrival shows the local time within the hour.
pub const LOOK_AGAIN: Duration = Duration::from_secs(30 * 60);

/// What the follower reaches of the device: where it is, and its clock.
pub trait Device: Send + Sync + 'static {
    /// Where the device is.
    fn locate(&self) -> Result<Location, String>;
    fn zone(&self) -> Option<String>;
    /// Set the device's zone.
    ///
    /// # Errors
    ///
    /// Why timedated did not take it.
    fn set_zone(&self, zone: &str) -> Result<(), String>;
    fn nearest_zone(&self, location: Location) -> Option<String>;
}

/// This device: the Location portal, timedated and the installed tzdata.
pub struct ThisDevice;

impl Device for ThisDevice {
    fn locate(&self) -> Result<Location, String> {
        match tilo_location::system()
            .locate()
            .recv()
            .map_err(|_| "the location service went away".to_owned())?
        {
            tilo_location::LocationOutcome::Located { lat, lon, .. } => Ok(Location {
                latitude: lat,
                longitude: lon,
            }),
            tilo_location::LocationOutcome::Unavailable(reason) => Err(reason),
        }
    }

    fn zone(&self) -> Option<String> {
        os_actions::get_time_sync_status().ok()?.timezone
    }

    fn set_zone(&self, zone: &str) -> Result<(), String> {
        os_actions::set_timezone(zone)
    }

    /// The zone of the installed tzdata's city nearest `location`.
    fn nearest_zone(&self, location: Location) -> Option<String> {
        settings_store::clock::ZONE_TABLES
            .iter()
            .find_map(|table| std::fs::read_to_string(table).ok())
            .and_then(|table| settings_store::clock::nearest_zone_in(location, &table))
    }
}

/// Look where the device is once and set the zone there, if it is another.
/// Returns the zone set, if one was.
///
/// # Errors
///
/// Why the device could not be located or its zone not set.
pub fn follow_once(device: &dyn Device) -> Result<Option<String>, String> {
    let location = device.locate()?;
    let Some(zone) = device.nearest_zone(location) else {
        return Ok(None);
    };
    if device.zone().as_deref() == Some(zone.as_str()) {
        return Ok(None);
    }
    device.set_zone(&zone)?;
    Ok(Some(zone))
}

/// Follow the device's location while the person has chosen so, until the
/// settings can no longer be watched. Runs on the session's runtime; every
/// look is a blocking job of its own.
pub async fn serve(device: std::sync::Arc<dyn Device>) {
    let (notify, mut changes) = tokio::sync::mpsc::unbounded_channel::<()>();
    let _watch = match settings_store::watch::watch_settings(move || {
        let _ = notify.send(());
    }) {
        Ok(watch) => watch,
        Err(error) => {
            tracing::warn!(target: "tilo::time", %error, "the time zone cannot follow the device: settings can't be watched");
            return;
        }
    };
    let mut following = false;
    loop {
        let wanted = settings_store::clock::AUTOMATIC_ZONE.get();
        // Turned on (or on at start): look now. Any other settings write
        // while on changes nothing; the next look is on the timer.
        if wanted && !following {
            look(&device).await;
        }
        following = wanted;
        if following {
            tokio::select! {
                change = changes.recv() => if change.is_none() { return },
                () = tokio::time::sleep(LOOK_AGAIN) => look(&device).await,
            }
        } else if changes.recv().await.is_none() {
            return;
        }
    }
}

async fn look(device: &std::sync::Arc<dyn Device>) {
    let device = std::sync::Arc::clone(device);
    match tokio::task::spawn_blocking(move || follow_once(device.as_ref())).await {
        Ok(Ok(Some(zone))) => {
            tracing::info!(target: "tilo::time", %zone, "time zone set from the device's location");
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => {
            tracing::info!(target: "tilo::time", %error, "the time zone stays: no location");
        }
        Err(error) => {
            tracing::warn!(target: "tilo::time", %error, "the time zone look ended without an answer");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fake {
        location: Result<Location, String>,
        zone: Mutex<Option<String>>,
        set: Mutex<Vec<String>>,
    }

    impl Device for Fake {
        fn locate(&self) -> Result<Location, String> {
            self.location.clone()
        }
        fn zone(&self) -> Option<String> {
            self.zone.lock().unwrap().clone()
        }
        fn set_zone(&self, zone: &str) -> Result<(), String> {
            self.set.lock().unwrap().push(zone.to_owned());
            *self.zone.lock().unwrap() = Some(zone.to_owned());
            Ok(())
        }
        fn nearest_zone(&self, location: Location) -> Option<String> {
            settings_store::clock::nearest_zone_in(
                location,
                "NL\t+5222+00454\tEurope/Amsterdam\n\
                 JP\t+353916+1394441\tAsia/Tokyo\n",
            )
        }
    }

    fn fake(location: Result<Location, String>, zone: &str) -> Fake {
        Fake {
            location,
            zone: Mutex::new(Some(zone.to_owned())),
            set: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn an_automatic_zone_is_the_zone_where_the_device_is_and_nothing_is_guessed() {
        let osaka = fake(
            Ok(Location {
                latitude: 34.69,
                longitude: 135.5,
            }),
            "Europe/Amsterdam",
        );
        assert_eq!(follow_once(&osaka), Ok(Some("Asia/Tokyo".to_owned())));
        assert_eq!(*osaka.set.lock().unwrap(), ["Asia/Tokyo"]);
        assert_eq!(follow_once(&osaka), Ok(None));
        assert_eq!(osaka.set.lock().unwrap().len(), 1);

        // No location: the zone stays as it is.
        let nowhere = fake(Err("no fix".to_owned()), "Europe/Amsterdam");
        assert!(follow_once(&nowhere).is_err());
        assert!(nowhere.set.lock().unwrap().is_empty());
        assert_eq!(nowhere.zone().as_deref(), Some("Europe/Amsterdam"));
    }
}
