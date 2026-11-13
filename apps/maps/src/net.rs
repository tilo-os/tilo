//! Live fetchers: Nominatim geocoding, OSRM routing, the Transitous/MOTIS planner and Overpass
//! POIs. Each request is a task on the app runtime and every answer comes back over one channel,
//! folded in during `sync`.

use crate::failure;
use std::fmt::Write as _;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::Value;

use crate::model::{
    Category, LatLon, Maneuver, Place, Poi, Route, RouteStep, TransitJourney, TransitLeg,
    TransitMode, TravelMode,
};

use crate::tiles::USER_AGENT;

/// Tolerance for Ramer–Douglas–Peucker on fetched route and leg geometry, applied on the fetch
/// worker: a few metres is imperceptible at map zoom (see [`crate::geo::simplify_polyline`]).
const GEOMETRY_SIMPLIFY_M: f64 = 3.0;

pub enum NetMsg {
    Search {
        query: String,
        results: Result<Vec<Place>, String>,
    },
    PaletteSearch {
        query: String,
        results: Result<Vec<Place>, String>,
    },
    EndpointSearch {
        origin: bool,
        query: String,
        results: Result<Vec<Place>, String>,
    },
    ReverseGeocode(Result<String, String>),
    Route(Result<Vec<Route>, String>),
    Transit(Result<Vec<TransitJourney>, String>),
    Pois {
        generation: u64,
        pois: Result<Vec<Poi>, String>,
    },
    RegionSearch {
        query: String,
        results: Result<Vec<RegionHit>, String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RegionHit {
    pub name: String,
    pub context: String,
    pub pos: LatLon,
    pub sw: LatLon,
    pub ne: LatLon,
}

#[derive(Clone, Debug, Default)]
pub struct TransitQuery {
    pub arrive_by: bool,
    pub time: Option<String>,
}

fn client() -> Result<&'static tilo_net::Client, String> {
    static CLIENT: OnceLock<Result<tilo_net::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(12))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

async fn fetch_json(url: &str) -> Result<Value, String> {
    let resp = client()?.get(url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let body = resp.bytes().await.map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

pub async fn geocode(query: &str) -> Result<Vec<Place>, String> {
    let q = ui::uri::encode_component(query);
    let url = format!(
        "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2&limit=8&addressdetails=1"
    );
    let v = fetch_json(&url).await?;
    let arr = v.as_array().ok_or("unexpected response")?;
    let out = arr.iter().filter_map(place_from_json).collect();
    Ok(out)
}

/// Geocode `query` for the download-by-place flow: each hit carries the Nominatim `boundingbox`, so
/// the caller downloads the place's area, not the screen rectangle.
pub async fn geocode_regions(query: &str) -> Result<Vec<RegionHit>, String> {
    let q = ui::uri::encode_component(query);
    let url = format!(
        "https://nominatim.openstreetmap.org/search?q={q}&format=jsonv2&limit=8&addressdetails=1"
    );
    let v = fetch_json(&url).await?;
    let arr = v.as_array().ok_or("unexpected response")?;
    Ok(arr.iter().filter_map(region_from_json).collect())
}

pub fn region_from_json(item: &Value) -> Option<RegionHit> {
    let place = place_from_json(item)?;
    let (sw, ne) = item["boundingbox"]
        .as_array()
        .and_then(|b| {
            let f = |i: usize| b.get(i)?.as_str()?.parse::<f64>().ok();
            Some((LatLon::new(f(0)?, f(2)?), LatLon::new(f(1)?, f(3)?)))
        })
        .unwrap_or_else(|| {
            let d = 0.006; // ~0.6 km half-extent
            (
                LatLon::new(place.pos.lat - d, place.pos.lon - d),
                LatLon::new(place.pos.lat + d, place.pos.lon + d),
            )
        });
    let context = place.address.split_once(',').map_or_else(
        || place.address.clone(),
        |(_, rest)| rest.trim().to_string(),
    );
    Some(RegionHit {
        name: place.name,
        context,
        pos: place.pos,
        sw,
        ne,
    })
}

pub fn place_from_json(item: &Value) -> Option<Place> {
    let lat = item["lat"].as_str().and_then(|s| s.parse::<f64>().ok())?;
    let lon = item["lon"].as_str().and_then(|s| s.parse::<f64>().ok())?;
    let display = item["display_name"].as_str().unwrap_or("").to_string();
    let name = place_label(item, &display);
    let class = item["class"]
        .as_str()
        .or_else(|| item["category"].as_str())
        .unwrap_or("");
    let kind = item["type"].as_str().unwrap_or("");
    let osm_type = item["osm_type"].as_str().unwrap_or("n");
    let osm_id = item["osm_id"].as_i64().unwrap_or(0);
    Some(Place::new(
        &format!("osm-{osm_type}-{osm_id}"),
        &name,
        Category::from_osm(class, kind),
        &display,
        LatLon::new(lat, lon),
    ))
}

/// A place name a person reads. `display_name`'s first fragment can be a bare house number ("18,
/// Theatinerstraße, …"), so this prefers the POI name, then `road [house_number]`, then the first
/// fragment that is not a number.
fn place_label(item: &Value, display: &str) -> String {
    if let Some(name) = item["name"].as_str().filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let addr = &item["address"];
    let road = addr["road"]
        .as_str()
        .or_else(|| addr["pedestrian"].as_str())
        .or_else(|| addr["footway"].as_str())
        .filter(|s| !s.is_empty());
    if let Some(road) = road {
        return match addr["house_number"].as_str().filter(|s| !s.is_empty()) {
            Some(hn) => format!("{road} {hn}"),
            None => road.to_string(),
        };
    }
    display
        .split(',')
        .map(str::trim)
        .find(|frag| !frag.is_empty() && !frag.chars().all(|c| c.is_ascii_digit()))
        .map_or_else(|| failure::t("maps-category-other"), str::to_owned)
}

pub fn overpass_url(south: f64, west: f64, north: f64, east: f64) -> String {
    let bbox = format!("{south:.5},{west:.5},{north:.5},{east:.5}");
    // `nwr` matches node, way and relation; `out center` gives each a point, and `out center 250`
    // caps the reply.
    let ql = format!(
        "[out:json][timeout:20];(\
nwr[\"amenity\"~\"^(restaurant|cafe|bar|pub|fast_food|biergarten|hospital|clinic|pharmacy|bank|cinema|theatre)$\"]({bbox});\
nwr[\"shop\"]({bbox});\
nwr[\"tourism\"~\"^(museum|gallery|artwork|attraction|viewpoint|hotel|hostel|guest_house)$\"]({bbox});\
nwr[\"leisure\"~\"^(park|garden)$\"]({bbox});\
nwr[\"railway\"~\"^(station|halt|tram_stop)$\"]({bbox});\
nwr[\"highway\"=\"bus_stop\"]({bbox});\
nwr[\"public_transport\"=\"station\"]({bbox});\
);out center 250;"
    );
    format!(
        "https://overpass-api.de/api/interpreter?data={}",
        ui::uri::encode_component(&ql)
    )
}

pub async fn overpass(south: f64, west: f64, north: f64, east: f64) -> Result<Vec<Poi>, String> {
    let url = overpass_url(south, west, north, east);
    let v = fetch_json(&url).await?;
    Ok(parse_overpass(&v))
}

pub fn parse_overpass(v: &Value) -> Vec<Poi> {
    let Some(elements) = v["elements"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in elements {
        let Some(poi) = poi_from_overpass(el) else {
            continue;
        };
        if seen.insert(poi.id.clone()) {
            out.push(poi);
        }
    }
    out
}

/// One Overpass element as a [`Poi`]; `None` without a name, a coordinate or a recognised category.
fn poi_from_overpass(el: &Value) -> Option<Poi> {
    let tags = &el["tags"];
    let name = tags["name"].as_str().filter(|s| !s.is_empty())?.to_string();
    let (lat, lon) = if let (Some(la), Some(lo)) = (el["lat"].as_f64(), el["lon"].as_f64()) {
        (la, lo)
    } else if let (Some(la), Some(lo)) = (
        el.get("center")
            .and_then(|c| c.get("lat"))
            .and_then(Value::as_f64),
        el.get("center")
            .and_then(|c| c.get("lon"))
            .and_then(Value::as_f64),
    ) {
        (la, lo)
    } else {
        return None;
    };
    let (class, kind) = overpass_class(tags)?;
    let category = Category::from_osm(class, kind);
    let el_type = el["type"].as_str().unwrap_or("node");
    let el_id = el["id"].as_i64().unwrap_or(0);
    Some(Poi {
        id: format!("osm-{el_type}-{el_id}"),
        name,
        category,
        pos: LatLon::new(lat, lon),
        address: compose_address(tags),
        opening_hours: tag_string(tags, "opening_hours"),
        website: tag_string(tags, "website").or_else(|| tag_string(tags, "contact:website")),
        phone: tag_string(tags, "phone").or_else(|| tag_string(tags, "contact:phone")),
        kind: tag_string(tags, "cuisine")
            .or_else(|| tag_string(tags, "shop"))
            .map(|s| s.replace('_', " ")),
    })
}

/// The most salient (class, kind) tag pair for an Overpass element.
fn overpass_class(tags: &Value) -> Option<(&str, &str)> {
    for class in [
        "amenity",
        "tourism",
        "shop",
        "leisure",
        "railway",
        "public_transport",
        "highway",
    ] {
        if let Some(kind) = tags[class].as_str().filter(|s| !s.is_empty()) {
            return Some((class, kind));
        }
    }
    None
}

fn compose_address(tags: &Value) -> String {
    let road = tag_string(tags, "addr:street");
    let hn = tag_string(tags, "addr:housenumber");
    let mut s = match (road, hn) {
        (Some(r), Some(h)) => format!("{r} {h}"),
        (Some(r), None) => r,
        _ => String::new(),
    };
    if let Some(city) = tag_string(tags, "addr:city") {
        if !s.is_empty() {
            s.push_str(", ");
        }
        s.push_str(&city);
    }
    s
}

fn tag_string(tags: &Value, key: &str) -> Option<String> {
    tags[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(std::string::ToString::to_string)
}

pub async fn routes(from: LatLon, to: LatLon, mode: TravelMode) -> Result<Vec<Route>, String> {
    let url = format!(
        "https://router.project-osrm.org/route/v1/{}/{:.6},{:.6};{:.6},{:.6}?overview=full&geometries=geojson&steps=true&alternatives=true",
        mode.osrm_profile(),
        from.lon,
        from.lat,
        to.lon,
        to.lat,
    );
    let v = fetch_json(&url).await?;
    if v.get("code").and_then(Value::as_str) != Some("Ok") {
        return Err(v
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("routing failed")
            .to_string());
    }
    let offered = v.get("routes").and_then(Value::as_array);
    let routes = offered
        .into_iter()
        .flatten()
        .filter_map(|r| route_from_json(r, mode, from, to))
        .collect::<Vec<_>>();
    if routes.is_empty() {
        return Err("the service offered no route".to_string());
    }
    Ok(routes)
}

fn route_from_json(value: &Value, mode: TravelMode, from: LatLon, to: LatLon) -> Option<Route> {
    let distance_m = value["distance"].as_f64().unwrap_or(0.0);
    let duration_s = value["duration"].as_f64().unwrap_or(0.0);
    let raw_geometry = value
        .get("geometry")
        .and_then(|g| g.get("coordinates"))
        .and_then(Value::as_array)
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
    if raw_geometry.len() < 2 {
        return None;
    }
    let geometry = crate::geo::simplify_polyline(&raw_geometry, GEOMETRY_SIMPLIFY_M);

    let mut steps = Vec::new();
    if let Some(legs) = value["legs"].as_array() {
        for leg in legs {
            if let Some(s) = leg["steps"].as_array() {
                for step in s {
                    let kind = step
                        .get("maneuver")
                        .and_then(|m| m.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let modifier = step
                        .get("maneuver")
                        .and_then(|m| m.get("modifier"))
                        .and_then(Value::as_str)
                        .unwrap_or("");
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

    Some(Route {
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
    let road = |key: &str| failure::t_args(key, &[("road", name.into())]);
    let named = |key: &str, plain: &str| {
        if name.is_empty() {
            failure::t(plain)
        } else {
            road(key)
        }
    };
    match m {
        Maneuver::Depart => named("maps-turn-head-out", "maps-turn-start"),
        Maneuver::Arrive => failure::t("maps-turn-arrive"),
        Maneuver::Continue | Maneuver::Merge => {
            named("maps-turn-continue-on", "maps-turn-continue")
        }
        Maneuver::Left => named("maps-turn-left-onto", "maps-turn-left"),
        Maneuver::SharpLeft => named("maps-turn-sharp-left-onto", "maps-turn-sharp-left"),
        Maneuver::SlightLeft => named("maps-turn-slight-left-onto", "maps-turn-slight-left"),
        Maneuver::Right => named("maps-turn-right-onto", "maps-turn-right"),
        Maneuver::SharpRight => named("maps-turn-sharp-right-onto", "maps-turn-sharp-right"),
        Maneuver::SlightRight => named("maps-turn-slight-right-onto", "maps-turn-slight-right"),
        Maneuver::Uturn => failure::t("maps-turn-uturn"),
        Maneuver::Roundabout => named("maps-turn-roundabout-onto", "maps-turn-roundabout"),
    }
}

pub async fn reverse_geocode(lat: f64, lon: f64) -> Result<String, String> {
    let url = format!(
        "https://nominatim.openstreetmap.org/reverse?lat={lat:.6}&lon={lon:.6}&format=jsonv2&zoom=16&addressdetails=1"
    );
    let v = fetch_json(&url).await?;
    if let Some(name) = v
        .get("name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Ok(name.to_string());
    }
    if let Some(display) = v
        .get("display_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let short: Vec<&str> = display.split(',').take(2).map(str::trim).collect();
        return Ok(short.join(", "));
    }
    Err("no address for this location".to_string())
}

pub fn transit_url(from: LatLon, to: LatLon, when: &TransitQuery) -> String {
    let mut url = format!(
        "https://api.transitous.org/api/v1/plan?fromPlace={:.6},{:.6}&toPlace={:.6},{:.6}&numItineraries=4",
        from.lat, from.lon, to.lat, to.lon
    );
    if let Some(time) = &when.time {
        let _ = write!(url, "&time={}", ui::uri::encode_component(time));
    }
    if when.arrive_by {
        url.push_str("&arriveBy=true");
    }
    url
}

/// Plan a journey through the Transitous MOTIS v3 API, anchored to the caller's depart/arrive
/// [`TransitQuery`]. Errors when the request fails or the answer does not parse.
pub async fn transit(
    from: LatLon,
    to: LatLon,
    when: &TransitQuery,
) -> Result<Vec<TransitJourney>, String> {
    let url = transit_url(from, to, when);
    let v = fetch_json(&url).await?;
    parse_transit(&v)
}

pub fn parse_transit(v: &Value) -> Result<Vec<TransitJourney>, String> {
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
                let from_name = leg
                    .get("from")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let to_name = leg
                    .get("to")
                    .and_then(|t| t.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let dep = hhmm(leg["startTime"].as_str().unwrap_or(""));
                let arr = hhmm(leg["endTime"].as_str().unwrap_or(""));
                let duration_min = (leg["duration"].as_f64().unwrap_or(0.0) / 60.0).round() as u32;
                let hex = |k: &str| {
                    leg[k]
                        .as_str()
                        .map(|s| s.trim_start_matches('#').to_string())
                        .filter(|s| s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit()))
                };
                legs.push(TransitLeg {
                    mode,
                    line,
                    headsign,
                    from_name,
                    to_name,
                    dep,
                    arr,
                    duration_min,
                    num_stops: leg["intermediateStops"]
                        .as_array()
                        .map_or(0, |a| a.len() as u32),
                    geometry: leg_geometry(leg),
                    color: hex("routeColor"),
                    text_color: hex("routeTextColor"),
                    agency: leg["agencyName"].as_str().unwrap_or("").to_string(),
                    realtime: leg["realTime"].as_bool().unwrap_or(false),
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

fn leg_geometry(leg: &Value) -> Vec<LatLon> {
    let g = &leg["legGeometry"];
    let Some(points) = g["points"].as_str().filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let precision = g["precision"].as_u64().unwrap_or(5) as u32;
    crate::geo::simplify_polyline(&decode_polyline(points, precision), GEOMETRY_SIMPLIFY_M)
}

pub fn decode_polyline(s: &str, precision: u32) -> Vec<LatLon> {
    let factor = 10f64.powi(precision as i32);
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut lat = 0i64;
    let mut lon = 0i64;
    let mut out = Vec::new();
    let read = |i: &mut usize| -> Option<i64> {
        let mut shift = 0u32;
        let mut result = 0i64;
        loop {
            let &byte = bytes.get(*i)?;
            let b = i64::from(byte) - 63;
            *i += 1;
            result |= (b & 0x1f) << shift;
            shift += 5;
            if b < 0x20 {
                break;
            }
        }
        Some(if result & 1 != 0 {
            !(result >> 1)
        } else {
            result >> 1
        })
    };
    while i < bytes.len() {
        let Some(dlat) = read(&mut i) else { break };
        let Some(dlon) = read(&mut i) else { break };
        lat += dlat;
        lon += dlon;
        out.push(LatLon::new(lat as f64 / factor, lon as f64 / factor));
    }
    out
}

fn hhmm(iso: &str) -> String {
    if let Some(t) = iso.find('T')
        && let Some(rest) = iso.get(t + 1..)
        && rest.len() >= 5
        && let Some(clock) = rest.get(..5)
    {
        return clock.to_string();
    }
    iso.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_parsers_extract_clocks_regions_steps_and_place_labels() {
        {
            assert_eq!(hhmm("2026-06-30T09:17:00+02:00"), "09:17");
            assert_eq!(hhmm("garbage"), "garbage");
        }

        {
            let item = serde_json::json!({
                "lat": "48.1371", "lon": "11.5754",
                "name": "Munich",
                "display_name": "Munich, Bavaria, Germany",
                "class": "boundary", "type": "administrative",
                "osm_type": "relation", "osm_id": 62428,
                "boundingbox": ["48.0616", "48.2482", "11.3608", "11.7229"]
            });
            let hit = region_from_json(&item).expect("a region with a coord + box");
            assert_eq!(hit.name, "Munich");
            assert!(
                (hit.sw.lat - 48.0616).abs() < 1e-6,
                "south from boundingbox[0]"
            );
            assert!(
                (hit.ne.lat - 48.2482).abs() < 1e-6,
                "north from boundingbox[1]"
            );
            assert!(
                (hit.sw.lon - 11.3608).abs() < 1e-6,
                "west from boundingbox[2]"
            );
            assert!(
                (hit.ne.lon - 11.7229).abs() < 1e-6,
                "east from boundingbox[3]"
            );
            assert!(hit.ne.lat > hit.sw.lat && hit.ne.lon > hit.sw.lon);
        }

        {
            let item = serde_json::json!({
                "lat": "48.1374", "lon": "11.5755",
                "name": "Marienplatz",
                "display_name": "Marienplatz, München",
                "class": "amenity", "type": "square",
                "osm_type": "node", "osm_id": 1
            });
            let hit = region_from_json(&item).expect("a region from a coord");
            assert!(
                hit.ne.lat > hit.sw.lat && hit.ne.lon > hit.sw.lon,
                "a non-degenerate box"
            );
            assert!(
                (hit.ne.lat - hit.sw.lat) < 0.05,
                "the fallback box is small"
            );
        }

        {
            assert_eq!(
                step_instruction(Maneuver::Left, "Main St"),
                "Turn left onto Main St"
            );
            assert_eq!(
                step_instruction(Maneuver::Arrive, ""),
                "Arrive at your destination"
            );
        }

        {
            let poi = serde_json::json!({
                "lat": "48.137", "lon": "11.575",
                "name": "Marienplatz",
                "display_name": "Marienplatz, Altstadt, München",
                "class": "tourism", "type": "attraction", "osm_type": "node", "osm_id": 1,
            });
            assert_eq!(place_from_json(&poi).unwrap().name, "Marienplatz");

            let address = serde_json::json!({
                "lat": "48.141", "lon": "11.577",
                "name": "",
                "display_name": "18, Theatinerstraße, Altstadt, München, 80333",
                "address": { "house_number": "18", "road": "Theatinerstraße", "city": "München" },
                "class": "place", "type": "house", "osm_type": "way", "osm_id": 2,
            });
            let p = place_from_json(&address).unwrap();
            assert_eq!(
                p.name, "Theatinerstraße 18",
                "must be street + number, never a bare '18'"
            );
            assert_ne!(p.name, "18");

            let bare = serde_json::json!({
                "lat": "48.14", "lon": "11.57",
                "name": "",
                "display_name": "12, Sendlinger Straße, München",
                "class": "place", "type": "house", "osm_type": "way", "osm_id": 3,
            });
            assert_eq!(place_from_json(&bare).unwrap().name, "Sendlinger Straße");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn transit_urls_polylines_and_legs_decode_robustly() {
        {
            let from = LatLon::new(48.10, 11.55);
            let to = LatLon::new(48.14, 11.58);
            let url = transit_url(from, to, &TransitQuery::default());
            assert!(url.contains("fromPlace=48.100000,11.550000"), "{url}");
            assert!(url.contains("toPlace=48.140000,11.580000"), "{url}");
            assert!(
                !url.contains("&time="),
                "depart-now must not pin a time: {url}"
            );
            assert!(
                !url.contains("arriveBy"),
                "depart-now is not arrive-by: {url}"
            );
        }

        {
            let from = LatLon::new(48.10, 11.55);
            let to = LatLon::new(48.14, 11.58);
            let when = TransitQuery {
                arrive_by: true,
                time: Some("2026-07-03T10:15:00+02:00".to_string()),
            };
            let url = transit_url(from, to, &when);
            assert!(
                url.contains("&time=2026-07-03T10%3A15%3A00%2B02%3A00"),
                "{url}"
            );
            assert!(url.contains("&arriveBy=true"), "{url}");
        }

        {
            let pts = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 5);
            assert_eq!(pts.len(), 3);
            assert!((pts[0].lat - 38.5).abs() < 1e-5, "{:?}", pts[0]);
            assert!((pts[0].lon - -120.2).abs() < 1e-5, "{:?}", pts[0]);
            assert!((pts[1].lat - 40.7).abs() < 1e-5, "{:?}", pts[1]);
            assert!((pts[1].lon - -120.95).abs() < 1e-5, "{:?}", pts[1]);
            assert!((pts[2].lat - 43.252).abs() < 1e-5, "{:?}", pts[2]);
            assert!((pts[2].lon - -126.453).abs() < 1e-5, "{:?}", pts[2]);
        }

        {
            assert!(decode_polyline("", 5).is_empty());
            assert!(decode_polyline("_", 6).is_empty());
        }

        {
            let body = serde_json::json!({
                "itineraries": [{
                    "startTime": "2026-07-04T09:12:00+02:00",
                    "endTime": "2026-07-04T09:26:00+02:00",
                    "duration": 840,
                    "transfers": 0,
                    "legs": [{
                        "mode": "SUBWAY",
                        "routeShortName": "U3",
                        "routeColor": "eb690b",
                        "routeTextColor": "#FFFFFF",
                        "agencyName": "MVG",
                        "realTime": true,
                        "headsign": "Moosach",
                        "from": {"name": "Goetheplatz"},
                        "to": {"name": "Marienplatz"},
                        "startTime": "2026-07-04T09:17:00+02:00",
                        "endTime": "2026-07-04T09:23:00+02:00",
                        "duration": 360,
                        "intermediateStops": [{}, {}, {}],
                        "legGeometry": {"points": "_p~iF~ps|U_ulLnnqC_mqNvxq`@", "precision": 5}
                    }]
                }]
            });
            let js = parse_transit(&body).expect("parses");
            assert_eq!(js.len(), 1);
            let leg = &js[0].legs[0];
            assert_eq!(leg.mode, TransitMode::Subway);
            assert_eq!(leg.line, "U3");
            assert_eq!(leg.num_stops, 3);
            assert_eq!(
                leg.geometry.len(),
                3,
                "legGeometry must decode onto the leg"
            );
            assert!((leg.geometry[0].lat - 38.5).abs() < 1e-5);
            assert_eq!(
                leg.color.as_deref(),
                Some("eb690b"),
                "GTFS routeColor parsed"
            );
            assert_eq!(
                leg.text_color.as_deref(),
                Some("FFFFFF"),
                "routeTextColor parsed, '#' stripped"
            );
            assert_eq!(leg.agency, "MVG");
            assert!(
                leg.realtime,
                "realTime flag parsed for the freshness signal"
            );
        }

        {
            let body = serde_json::json!({
                "itineraries": [{
                    "duration": 300,
                    "legs": [{"mode": "WALK", "from": {"name": "A"}, "to": {"name": "B"}}]
                }]
            });
            let js = parse_transit(&body).expect("parses");
            assert!(js[0].legs[0].geometry.is_empty());
        }
    }

    #[test]
    fn overpass_query_encodes_bbox_and_results_map_to_categories() {
        {
            let url = overpass_url(48.10, 11.55, 48.15, 11.60);
            assert!(url.starts_with("https://overpass-api.de/api/interpreter?data="));
            assert!(
                url.contains("48.10000%2C11.55000%2C48.15000%2C11.60000"),
                "{url}"
            );
            assert!(url.contains("restaurant"));
            assert!(
                url.contains("out%20center%20250"),
                "server-side cap present: {url}"
            );
        }

        {
            let body = serde_json::json!({
                "elements": [
                    { // a node café with rich tags
                        "type": "node", "id": 1, "lat": 48.137, "lon": 11.575,
                        "tags": {
                            "name": "Café Glück", "amenity": "cafe", "cuisine": "coffee_shop",
                            "opening_hours": "Mo-Fr 08:00-18:00", "website": "https://gluck.example",
                            "phone": "+49 89 123", "addr:street": "Theatinerstraße", "addr:housenumber": "18",
                            "addr:city": "München"
                        }
                    },
                    { // a way museum with a center point
                        "type": "way", "id": 2, "center": {"lat": 48.140, "lon": 11.578},
                        "tags": {"name": "Stadtmuseum", "tourism": "museum"}
                    },
                    { // unnamed node — dropped (noise)
                        "type": "node", "id": 3, "lat": 48.1, "lon": 11.5, "tags": {"amenity": "bench"}
                    },
                    { // no recognised category — dropped
                        "type": "node", "id": 4, "lat": 48.1, "lon": 11.5, "tags": {"name": "Nowhere"}
                    },
                ]
            });
            let pois = parse_overpass(&body);
            assert_eq!(pois.len(), 2, "only named, categorised elements survive");

            let cafe = &pois[0];
            assert_eq!(cafe.id, "osm-node-1");
            assert_eq!(cafe.name, "Café Glück");
            assert_eq!(cafe.category, Category::Cafe);
            assert_eq!(cafe.address, "Theatinerstraße 18, München");
            assert_eq!(cafe.opening_hours.as_deref(), Some("Mo-Fr 08:00-18:00"));
            assert_eq!(cafe.website.as_deref(), Some("https://gluck.example"));
            assert_eq!(cafe.phone.as_deref(), Some("+49 89 123"));

            let museum = &pois[1];
            assert_eq!(museum.id, "osm-way-2");
            assert_eq!(museum.category, Category::Museum);
            assert!(
                (museum.pos.lat - 48.140).abs() < 1e-9,
                "way center becomes the position"
            );
        }
    }
}
