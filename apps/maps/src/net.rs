//! Live data fetchers — Nominatim geocoding, OSRM routing, and the
//! Transitous/MOTIS public-transport planner. All requests are blocking and
//! run on background threads spawned by the app; results flow back over an
//! `mpsc` channel and are folded into the model during `sync`.
//!
//! None of this runs under `--shots` (no event loop) — the screenshot uses
//! the bundled sample data instead.

use std::time::Duration;

use serde_json::Value;

use crate::model::{
    Category, LatLon, Maneuver, Place, Route, RouteStep, TransitJourney, TransitLeg, TransitMode,
    TravelMode,
};

const USER_AGENT: &str = "qOS-Maps/0.1 (https://qos.example; native maps app)";

/// A completed background request handed back to the app.
pub enum NetMsg {
    Search {
        query: String,
        results: Result<Vec<Place>, String>,
    },
    /// Geocoding for one of the Directions endpoints (From / To). `origin` is
    /// `true` for the "From" field, `false` for "To".
    EndpointSearch {
        origin: bool,
        query: String,
        results: Result<Vec<Place>, String>,
    },
    /// Reverse-geocoded label for a coordinate (used to name a device-location
    /// origin with a human place instead of raw lat/lon).
    ReverseGeocode(Result<String, String>),
    Route(Result<Route, String>),
    Transit(Result<Vec<TransitJourney>, String>),
}

/// Depart/arrive anchoring for a transit query. `time` is an RFC-3339 wall
/// clock; `None` means "now" (the planner's own current time).
#[derive(Clone, Debug, Default)]
pub struct TransitQuery {
    pub arrive_by: bool,
    pub time: Option<String>,
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(12))
        .build()
        .expect("reqwest client builds")
}

fn fetch_json(url: &str) -> Result<Value, String> {
    let resp = client().get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    resp.json::<Value>().map_err(|e| e.to_string())
}

/// Geocode a free-text query via Nominatim.
pub fn geocode(query: &str) -> Result<Vec<Place>, String> {
    let q = urlencode(query);
    let url = format!(
        "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2&limit=8&addressdetails=1"
    );
    let v = fetch_json(&url)?;
    let arr = v.as_array().ok_or("unexpected response")?;
    let mut out = Vec::new();
    for item in arr {
        let lat = item["lat"].as_str().and_then(|s| s.parse::<f64>().ok());
        let lon = item["lon"].as_str().and_then(|s| s.parse::<f64>().ok());
        let (Some(lat), Some(lon)) = (lat, lon) else {
            continue;
        };
        let display = item["display_name"].as_str().unwrap_or("").to_string();
        let name = item["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| display.split(',').next().unwrap_or("Place").to_string());
        let class = item["class"].as_str().or_else(|| item["category"].as_str()).unwrap_or("");
        let kind = item["type"].as_str().unwrap_or("");
        let osm_type = item["osm_type"].as_str().unwrap_or("n");
        let osm_id = item["osm_id"].as_i64().unwrap_or(0);
        let mut place = Place::new(
            &format!("osm-{osm_type}-{osm_id}"),
            &name,
            Category::from_osm(class, kind),
            &display,
            LatLon::new(lat, lon),
        );
        if let Some(imp) = item["importance"].as_f64() {
            place.rating = Some((3.5 + imp.clamp(0.0, 1.0) * 1.5) as f32);
        }
        out.push(place);
    }
    Ok(out)
}

/// Route between two coordinates with the OSRM public demo server.
pub fn route(from: LatLon, to: LatLon, mode: TravelMode) -> Result<Route, String> {
    let url = format!(
        "https://router.project-osrm.org/route/v1/{}/{:.6},{:.6};{:.6},{:.6}?overview=full&geometries=geojson&steps=true",
        mode.osrm_profile(),
        from.lon,
        from.lat,
        to.lon,
        to.lat,
    );
    let v = fetch_json(&url)?;
    if v["code"].as_str() != Some("Ok") {
        return Err(v["code"].as_str().unwrap_or("routing failed").to_string());
    }
    let r0 = &v["routes"][0];
    let distance_m = r0["distance"].as_f64().unwrap_or(0.0);
    let duration_s = r0["duration"].as_f64().unwrap_or(0.0);
    let geometry = r0["geometry"]["coordinates"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|c| {
                    let lon = c[0].as_f64()?;
                    let lat = c[1].as_f64()?;
                    Some(LatLon::new(lat, lon))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut steps = Vec::new();
    if let Some(legs) = r0["legs"].as_array() {
        for leg in legs {
            if let Some(s) = leg["steps"].as_array() {
                for step in s {
                    let kind = step["maneuver"]["type"].as_str().unwrap_or("");
                    let modifier = step["maneuver"]["modifier"].as_str().unwrap_or("");
                    let name = step["name"].as_str().unwrap_or("");
                    let dist = step["distance"].as_f64().unwrap_or(0.0);
                    let maneuver = Maneuver::from_osrm(kind, modifier);
                    let instruction = step_instruction(maneuver, name);
                    steps.push(RouteStep {
                        instruction,
                        distance_m: dist,
                        maneuver,
                    });
                }
            }
        }
    }

    Ok(Route {
        mode,
        distance_m,
        duration_s,
        geometry,
        steps,
        from,
        to,
    })
}

fn step_instruction(m: Maneuver, name: &str) -> String {
    let road = if name.is_empty() {
        String::new()
    } else {
        format!(" onto {name}")
    };
    match m {
        Maneuver::Depart => {
            if name.is_empty() {
                "Start".to_string()
            } else {
                format!("Head out on {name}")
            }
        }
        Maneuver::Arrive => "Arrive at your destination".to_string(),
        Maneuver::Continue | Maneuver::Merge => {
            if name.is_empty() {
                "Continue".to_string()
            } else {
                format!("Continue on {name}")
            }
        }
        Maneuver::Left => format!("Turn left{road}"),
        Maneuver::SharpLeft => format!("Sharp left{road}"),
        Maneuver::SlightLeft => format!("Slight left{road}"),
        Maneuver::Right => format!("Turn right{road}"),
        Maneuver::SharpRight => format!("Sharp right{road}"),
        Maneuver::SlightRight => format!("Slight right{road}"),
        Maneuver::Uturn => "Make a U-turn".to_string(),
        Maneuver::Roundabout => format!("At the roundabout, take the exit{road}"),
    }
}

/// Reverse-geocode a coordinate to a human place label via Nominatim. Used to
/// name a device-location origin ("Current location" → a real street/place).
pub fn reverse_geocode(lat: f64, lon: f64) -> Result<String, String> {
    let url = format!(
        "https://nominatim.openstreetmap.org/reverse?lat={lat:.6}&lon={lon:.6}&format=jsonv2&zoom=16&addressdetails=1"
    );
    let v = fetch_json(&url)?;
    // Prefer the specific `name`, then a short leading fragment of the full
    // display name, so the origin label stays compact.
    if let Some(name) = v["name"].as_str().filter(|s| !s.is_empty()) {
        return Ok(name.to_string());
    }
    if let Some(display) = v["display_name"].as_str().filter(|s| !s.is_empty()) {
        let short: Vec<&str> = display.split(',').take(2).map(|s| s.trim()).collect();
        return Ok(short.join(", "));
    }
    Err("no address for this location".to_string())
}

/// Build the Transitous MOTIS `plan` URL for a journey, honouring the
/// depart/arrive time anchor. Split out (and pure) so the time-picker wiring is
/// unit-testable without a network round-trip.
pub fn transit_url(from: LatLon, to: LatLon, when: &TransitQuery) -> String {
    let mut url = format!(
        "https://api.transitous.org/api/v1/plan?fromPlace={:.6},{:.6}&toPlace={:.6},{:.6}&numItineraries=4",
        from.lat, from.lon, to.lat, to.lon
    );
    if let Some(time) = &when.time {
        url.push_str(&format!("&time={}", urlencode(time)));
    }
    if when.arrive_by {
        url.push_str("&arriveBy=true");
    }
    url
}

/// Plan a public-transport journey via the Transitous MOTIS v3 API, anchored to
/// the caller's depart/arrive [`TransitQuery`].
pub fn transit(from: LatLon, to: LatLon, when: &TransitQuery) -> Result<Vec<TransitJourney>, String> {
    let url = transit_url(from, to, when);
    let v = fetch_json(&url)?;
    let its = v["itineraries"].as_array().ok_or("no itineraries")?;
    let mut out = Vec::new();
    for it in its {
        let dep = hhmm(it["startTime"].as_str().unwrap_or(""));
        let arr = hhmm(it["endTime"].as_str().unwrap_or(""));
        let duration_min = (it["duration"].as_f64().unwrap_or(0.0) / 60.0).round() as u32;
        let transfers = it["transfers"].as_u64().unwrap_or(0) as u32;
        let mut legs = Vec::new();
        if let Some(ls) = it["legs"].as_array() {
            for leg in ls {
                let mode = TransitMode::from_motis(leg["mode"].as_str().unwrap_or("WALK"));
                let line = leg["routeShortName"]
                    .as_str()
                    .or_else(|| leg["route"].as_str())
                    .unwrap_or("")
                    .to_string();
                let headsign = leg["headsign"].as_str().unwrap_or("").to_string();
                let from_name = leg["from"]["name"].as_str().unwrap_or("").to_string();
                let to_name = leg["to"]["name"].as_str().unwrap_or("").to_string();
                let dep = hhmm(leg["startTime"].as_str().unwrap_or(""));
                let arr = hhmm(leg["endTime"].as_str().unwrap_or(""));
                let duration_min = (leg["duration"].as_f64().unwrap_or(0.0) / 60.0).round() as u32;
                legs.push(TransitLeg {
                    mode,
                    line,
                    headsign,
                    from_name,
                    to_name,
                    dep,
                    arr,
                    duration_min,
                    num_stops: leg["intermediateStops"].as_array().map(|a| a.len() as u32).unwrap_or(0),
                    geometry: vec![],
                });
            }
        }
        out.push(TransitJourney {
            dep,
            arr,
            duration_min,
            transfers,
            legs,
        });
    }
    Ok(out)
}

/// Extract `HH:MM` from an ISO-8601 timestamp; best-effort.
fn hhmm(iso: &str) -> String {
    if let Some(t) = iso.find('T') {
        let rest = &iso[t + 1..];
        if rest.len() >= 5 {
            return rest[..5].to_string();
        }
    }
    iso.to_string()
}

/// Minimal percent-encoding for query strings (space + reserved chars).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hhmm_extracts_clock() {
        assert_eq!(hhmm("2026-06-30T09:17:00+02:00"), "09:17");
        assert_eq!(hhmm("garbage"), "garbage");
    }

    #[test]
    fn urlencode_basic() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn step_instruction_formats() {
        assert_eq!(step_instruction(Maneuver::Left, "Main St"), "Turn left onto Main St");
        assert_eq!(step_instruction(Maneuver::Arrive, ""), "Arrive at your destination");
    }

    #[test]
    fn transit_url_depart_now_omits_time() {
        let from = LatLon::new(48.10, 11.55);
        let to = LatLon::new(48.14, 11.58);
        let url = transit_url(from, to, &TransitQuery::default());
        assert!(url.contains("fromPlace=48.100000,11.550000"), "{url}");
        assert!(url.contains("toPlace=48.140000,11.580000"), "{url}");
        assert!(!url.contains("&time="), "depart-now must not pin a time: {url}");
        assert!(!url.contains("arriveBy"), "depart-now is not arrive-by: {url}");
    }

    #[test]
    fn transit_url_arrive_by_carries_time_and_flag() {
        let from = LatLon::new(48.10, 11.55);
        let to = LatLon::new(48.14, 11.58);
        let when = TransitQuery {
            arrive_by: true,
            time: Some("2026-07-03T10:15:00+02:00".to_string()),
        };
        let url = transit_url(from, to, &when);
        // The colons/plus of the timestamp must be percent-encoded into the URL.
        assert!(url.contains("&time=2026-07-03T10%3A15%3A00%2B02%3A00"), "{url}");
        assert!(url.contains("&arriveBy=true"), "{url}");
    }
}
