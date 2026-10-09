//! Typed Automerge schema for Maps' durable collaborative resources.
//!
//! The `maps_v2` map has named fields for each durable resource. It never uses
//! a JSON/native-tree projection as authority. Search results and viewport POIs are local,
//! derived caches and deliberately have no CRDT representation.

#![allow(
    clippy::collapsible_if,
    clippy::field_reassign_with_default,
    clippy::format_collect,
    clippy::many_single_char_names,
    clippy::match_wildcard_for_single_variants,
    clippy::needless_collect,
    clippy::semicolon_if_nothing_returned,
    reason = "the schema keeps each CRDT reader/writer adjacent to its resource field"
)]

use crate::model::{
    Category, LatLon, MapModel, Place, Route, TransitJourney, TransitTime, TransitTimeMode,
    TravelMode,
};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use serde::{Deserialize, Serialize};
use ui::doc::{read_bool, read_number, read_scalar, read_str_or_text, read_u64};

const ROOT_KEY: &str = "maps_v2";

/// One atomic accepted-navigation value. A route and all of the endpoint
/// context that gives it meaning must win or lose together when two replicas
/// accept different planner replies concurrently; storing their fields as
/// independent registers can otherwise produce a route to one place labelled
/// as a route to another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum ActivePlan {
    None,
    Route {
        route: Route,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
        mode: TravelMode,
    },
    Transit {
        transit: Vec<TransitJourney>,
        origin: Option<LatLon>,
        origin_name: String,
        dest: Option<LatLon>,
        dest_name: String,
    },
}

pub struct MapsDoc;

impl MapsDoc {
    pub fn load(doc: &AutoCommit) -> Option<MapModel> {
        Self::load_v2(doc)
    }

    /// A test's whole-model writer: every edit of a live map is one of its
    /// actions. Ranked results, drafts, status, camera, selection, panel and
    /// transit-row choice are presentation/derived data and are not mirrored
    /// here.
    #[cfg(test)]
    pub fn save(doc: &mut AutoCommit, m: &MapModel) {
        let r = Self::root(doc);
        Self::places(doc, &r, &m.places);
        Self::recents(doc, &r, &m.recents);
        Self::text_put(doc, &r, "mode", travel_name(m.mode));
        Self::opt_point(doc, &r, "origin", m.origin);
        Self::text_put(doc, &r, "origin_name", &m.origin_name);
        Self::opt_point(doc, &r, "dest", m.dest);
        Self::text_put(doc, &r, "dest_name", &m.dest_name);
        Self::text_put(doc, &r, "transit_time_mode", time_name(m.transit_time.mode));
        Self::i(
            doc,
            &r,
            "transit_time_offset",
            i64::from(m.transit_time.offset_min),
        );
        let route_plan = m
            .route
            .as_ref()
            .map_or(ActivePlan::None, |route| ActivePlan::Route {
                route: route.clone(),
                origin: m.origin,
                origin_name: m.origin_name.clone(),
                dest: m.dest,
                dest_name: m.dest_name.clone(),
                mode: m.mode,
            });
        let transit_plan = if m.transit.is_empty() {
            ActivePlan::None
        } else {
            ActivePlan::Transit {
                transit: m.transit.clone(),
                origin: m.origin,
                origin_name: m.origin_name.clone(),
                dest: m.dest,
                dest_name: m.dest_name.clone(),
            }
        };
        Self::active_plan_put(doc, &r, "route_plan", &route_plan);
        Self::active_plan_put(doc, &r, "transit_plan", &transit_plan);
    }

    pub fn set_place_saved(doc: &mut AutoCommit, id: &str, saved: bool) {
        if let Some(row) = Self::row(doc, "places", id)
            && !Self::dead(doc, &row)
        {
            Self::b(doc, &row, "saved", saved)
        }
    }
    pub fn upsert_places(doc: &mut AutoCommit, items: &[Place]) {
        let r = Self::root(doc);
        let rows = Self::map_make(doc, &r, "places");
        for p in items {
            let row = Self::map_make(doc, &rows, &p.id);
            Self::place_put(doc, &row, p);
            Self::b(doc, &row, "deleted", false)
        }
    }
    pub fn commit_search(doc: &mut AutoCommit, q: &str) {
        let q = q.trim();
        if q.is_empty() {
            return;
        }
        let r = Self::root(doc);
        let mut v = Self::recents_get(doc, &r);
        v.retain(|x| !x.eq_ignore_ascii_case(q));
        v.insert(0, q.into());
        v.truncate(8);
        Self::recents(doc, &r, &v)
    }
    pub fn clear_recents(doc: &mut AutoCommit) {
        let r = Self::root(doc);
        Self::all_dead(doc, &r, "recents")
    }
    pub fn clear_route(doc: &mut AutoCommit) {
        let r = Self::root(doc);
        Self::active_plan_put(doc, &r, "route_plan", &ActivePlan::None)
    }
    pub fn clear_transit(doc: &mut AutoCommit) {
        let r = Self::root(doc);
        Self::active_plan_put(doc, &r, "transit_plan", &ActivePlan::None)
    }
    pub fn set_active_route(
        doc: &mut AutoCommit,
        route: &Route,
        origin: Option<LatLon>,
        origin_name: &str,
        dest: Option<LatLon>,
        dest_name: &str,
        mode: TravelMode,
    ) {
        let r = Self::root(doc);
        Self::text_put(doc, &r, "mode", travel_name(mode));
        Self::active_plan_put(
            doc,
            &r,
            "route_plan",
            &ActivePlan::Route {
                route: route.clone(),
                origin,
                origin_name: origin_name.to_string(),
                dest,
                dest_name: dest_name.to_string(),
                mode,
            },
        )
    }

    pub fn set_active_transit(
        doc: &mut AutoCommit,
        transit: &[TransitJourney],
        origin: Option<LatLon>,
        origin_name: &str,
        dest: Option<LatLon>,
        dest_name: &str,
    ) {
        let r = Self::root(doc);
        Self::text_put(doc, &r, "mode", travel_name(TravelMode::Transit));
        Self::active_plan_put(
            doc,
            &r,
            "transit_plan",
            &ActivePlan::Transit {
                transit: transit.to_vec(),
                origin,
                origin_name: origin_name.to_string(),
                dest,
                dest_name: dest_name.to_string(),
            },
        )
    }

    pub fn set_travel_mode(doc: &mut AutoCommit, mode: TravelMode) {
        let r = Self::root(doc);
        Self::text_put(doc, &r, "mode", travel_name(mode))
    }

    pub fn set_transit_time(doc: &mut AutoCommit, value: TransitTime) {
        let r = Self::root(doc);
        if let Ok(json) = serde_json::to_string(&value) {
            let _ = doc.put(&r, "transit_time", json);
        }
    }

    fn load_v2(doc: &AutoCommit) -> Option<MapModel> {
        let r = Self::existing(doc)?;
        let mut m = MapModel::default();
        m.places = Self::places_get(doc, &r);
        m.recents = Self::recents_get(doc, &r);
        m.mode = read_str_or_text(doc, &r, "mode")
            .as_deref()
            .and_then(travel)
            .unwrap_or(TravelMode::Car);
        m.origin = Self::point_get(doc, &r, "origin");
        m.origin_name =
            read_str_or_text(doc, &r, "origin_name").unwrap_or_else(|| "Map centre".into());
        m.dest = Self::point_get(doc, &r, "dest");
        m.dest_name = read_str_or_text(doc, &r, "dest_name").unwrap_or_default();
        m.transit_time = TransitTime {
            mode: read_str_or_text(doc, &r, "transit_time_mode")
                .as_deref()
                .and_then(time)
                .unwrap_or(TransitTimeMode::Depart),
            offset_min: Self::i_get(doc, &r, "transit_time_offset").unwrap_or(0) as i32,
        };
        if let Some(value) = read_str_or_text(doc, &r, "transit_time")
            .and_then(|json| serde_json::from_str(&json).ok())
        {
            m.transit_time = value;
        }
        let mut route_context = None;
        if let Some(active) = Self::active_plan_get(doc, &r, "route_plan") {
            match active {
                ActivePlan::None => {
                    m.route = None;
                }
                ActivePlan::Route {
                    route,
                    origin,
                    origin_name,
                    dest,
                    dest_name,
                    mode,
                } => {
                    m.route = Some(route);
                    route_context = Some((origin, origin_name, dest, dest_name, mode));
                }
                ActivePlan::Transit { .. } => {}
            }
        }
        let mut transit_context = None;
        if let Some(active) = Self::active_plan_get(doc, &r, "transit_plan") {
            match active {
                ActivePlan::None => m.transit.clear(),
                ActivePlan::Route { .. } => {}
                ActivePlan::Transit {
                    transit,
                    origin,
                    origin_name,
                    dest,
                    dest_name,
                } => {
                    m.transit = transit;
                    transit_context = Some((origin, origin_name, dest, dest_name));
                }
            }
        }
        if m.mode.is_transit() {
            if let Some((origin, origin_name, dest, dest_name)) = transit_context {
                m.origin = origin;
                m.origin_name = origin_name;
                m.dest = dest;
                m.dest_name = dest_name;
            }
        } else if let Some((origin, origin_name, dest, dest_name, _accepted_mode)) = route_context {
            m.origin = origin;
            m.origin_name = origin_name;
            m.dest = dest;
            m.dest_name = dest_name;
        }
        Some(m)
    }

    fn active_plan_put(doc: &mut AutoCommit, root: &ObjId, key: &str, plan: &ActivePlan) {
        if let Ok(json) = serde_json::to_string(plan) {
            // A scalar string is intentional: Automerge chooses one complete
            // concurrent value deterministically instead of interleaving the
            // nested route steps from two planner replies.
            let _ = doc.put(root, key, json);
        }
    }

    fn active_plan_get(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<ActivePlan> {
        read_str_or_text(doc, root, key).and_then(|json| serde_json::from_str(&json).ok())
    }
    fn root(doc: &mut AutoCommit) -> ObjId {
        Self::map_make(doc, &ROOT, ROOT_KEY)
    }
    fn existing(doc: &AutoCommit) -> Option<ObjId> {
        match doc.get(&ROOT, ROOT_KEY).ok().flatten() {
            Some((Value::Object(ObjType::Map), id)) => Some(id),
            _ => None,
        }
    }
    fn map_make(doc: &mut AutoCommit, p: &ObjId, k: &str) -> ObjId {
        match doc.get(p, k) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(p, k, ObjType::Map)
                .unwrap_or_else(|_| p.clone()),
        }
    }
    fn map(doc: &AutoCommit, p: &ObjId, k: &str) -> Option<ObjId> {
        match doc.get(p, k).ok().flatten() {
            Some((Value::Object(ObjType::Map), id)) => Some(id),
            _ => None,
        }
    }
    fn row(doc: &mut AutoCommit, collection: &str, id: &str) -> Option<ObjId> {
        let r = Self::root(doc);
        let rows = Self::map_make(doc, &r, collection);
        match doc.get(&rows, id).ok().flatten() {
            Some((Value::Object(ObjType::Map), x)) => Some(x),
            _ => None,
        }
    }
    fn dead(doc: &AutoCommit, row: &ObjId) -> bool {
        Self::conflict_true(doc, row, "deleted")
    }
    fn conflict_true(doc: &AutoCommit, p: &ObjId, k: &str) -> bool {
        doc.get_all(p,k).is_ok_and(|v|v.into_iter().any(|(v,_)|matches!(v,Value::Scalar(v)if matches!(v.as_ref(),ScalarValue::Boolean(true)))))
    }
    fn all_dead(doc: &mut AutoCommit, r: &ObjId, k: &str) {
        let rows = Self::map_make(doc, r, k);
        for id in doc.keys(&rows).collect::<Vec<_>>() {
            if let Some(row) = Self::map(doc, &rows, &id) {
                Self::b(doc, &row, "deleted", true)
            }
        }
    }

    #[cfg(test)]
    fn places(doc: &mut AutoCommit, r: &ObjId, v: &[Place]) {
        let rows = Self::map_make(doc, r, "places");
        let keep: std::collections::BTreeSet<_> = v.iter().map(|x| x.id.as_str()).collect();
        for x in v {
            let row = Self::map_make(doc, &rows, &x.id);
            Self::place_put(doc, &row, x);
            Self::b(doc, &row, "deleted", false)
        }
        for id in doc.keys(&rows).collect::<Vec<_>>() {
            if !keep.contains(id.as_str()) {
                if let Some(row) = Self::map(doc, &rows, &id) {
                    Self::b(doc, &row, "deleted", true)
                }
            }
        }
    }
    fn place_put(doc: &mut AutoCommit, r: &ObjId, x: &Place) {
        Self::text_put(doc, r, "id", &x.id);
        Self::text_put(doc, r, "name", &x.name);
        Self::text_put(doc, r, "category", category_name(x.category));
        Self::text_put(doc, r, "address", &x.address);
        Self::point(doc, r, "pos", x.pos);
        Self::b(doc, r, "saved", x.saved)
    }
    fn places_get(doc: &AutoCommit, r: &ObjId) -> Vec<Place> {
        let Some(rows) = Self::map(doc, r, "places") else {
            return vec![];
        };
        let mut v = doc
            .keys(&rows)
            .filter_map(|id| {
                let x = Self::map(doc, &rows, &id)?;
                (!Self::dead(doc, &x))
                    .then(|| Self::place_get(doc, &x))
                    .flatten()
            })
            .collect::<Vec<_>>();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }
    fn place_get(doc: &AutoCommit, r: &ObjId) -> Option<Place> {
        Some(Place {
            id: read_str_or_text(doc, r, "id")?,
            name: read_str_or_text(doc, r, "name")?,
            category: category(&read_str_or_text(doc, r, "category")?)?,
            address: read_str_or_text(doc, r, "address").unwrap_or_default(),
            pos: Self::point_get(doc, r, "pos")?,
            saved: read_bool(doc, r, "saved").unwrap_or(false),
        })
    }
    fn recents(doc: &mut AutoCommit, r: &ObjId, v: &[String]) {
        let rows = Self::map_make(doc, r, "recents");
        let keep: std::collections::BTreeSet<_> = v.iter().map(|q| recent_id(q)).collect();
        for (n, q) in v.iter().enumerate() {
            let id = recent_id(q);
            let row = Self::map_make(doc, &rows, &id);
            Self::text_put(doc, &row, "text", q);
            Self::u(doc, &row, "rank", n as u64);
            Self::b(doc, &row, "deleted", false)
        }
        for id in doc.keys(&rows).collect::<Vec<_>>() {
            if !keep.contains(&id) {
                if let Some(row) = Self::map(doc, &rows, &id) {
                    Self::b(doc, &row, "deleted", true)
                }
            }
        }
    }
    fn recents_get(doc: &AutoCommit, r: &ObjId) -> Vec<String> {
        let Some(rows) = Self::map(doc, r, "recents") else {
            return vec![];
        };
        let mut v = doc
            .keys(&rows)
            .filter_map(|id| {
                let x = Self::map(doc, &rows, &id)?;
                (!Self::dead(doc, &x))
                    .then(|| {
                        Some((
                            read_u64(doc, &x, "rank").unwrap_or(u64::MAX),
                            id,
                            read_str_or_text(doc, &x, "text")?,
                        ))
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        v.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        v.into_iter().map(|(_, _, q)| q).collect()
    }

    fn point(doc: &mut AutoCommit, p: &ObjId, k: &str, x: LatLon) {
        let r = Self::map_make(doc, p, k);
        Self::f(doc, &r, "lat", x.lat);
        Self::f(doc, &r, "lon", x.lon)
    }
    #[cfg(test)]
    fn opt_point(doc: &mut AutoCommit, p: &ObjId, k: &str, x: Option<LatLon>) {
        if let Some(x) = x {
            Self::point(doc, p, k, x)
        } else {
            let _ = doc.delete(p, k);
        }
    }
    fn point_get(doc: &AutoCommit, p: &ObjId, k: &str) -> Option<LatLon> {
        let r = Self::map(doc, p, k)?;
        Some(LatLon::new(
            read_number(doc, &r, "lat")?,
            read_number(doc, &r, "lon")?,
        ))
    }
    fn text_put(doc: &mut AutoCommit, p: &ObjId, k: &str, v: &str) {
        match doc.get(p, k) {
            Ok(Some((Value::Object(ObjType::Text), id))) => {
                ui::doc::splice_text_diffed(doc, &id, v);
            }
            _ => {
                if let Ok(id) = doc.put_object(p, k, ObjType::Text) {
                    ui::doc::splice_text_diffed(doc, &id, v);
                }
            }
        }
    }
    fn b(doc: &mut AutoCommit, p: &ObjId, k: &str, v: bool) {
        if read_bool(doc, p, k) != Some(v) {
            let _ = doc.put(p, k, v);
        }
    }
    fn u(doc: &mut AutoCommit, p: &ObjId, k: &str, v: u64) {
        if read_u64(doc, p, k) != Some(v) {
            let _ = doc.put(p, k, v);
        }
    }
    #[cfg(test)]
    fn i(doc: &mut AutoCommit, p: &ObjId, k: &str, v: i64) {
        if Self::i_get(doc, p, k) != Some(v) {
            let _ = doc.put(p, k, v);
        }
    }
    fn f(doc: &mut AutoCommit, p: &ObjId, k: &str, v: f64) {
        if read_number(doc, p, k).is_none_or(|x| (x - v).abs() > f64::EPSILON) {
            let _ = doc.put(p, k, v);
        }
    }
    /// An `Int`, or a `Uint` that fits in `i64`.
    fn i_get(doc: &AutoCommit, p: &ObjId, k: &str) -> Option<i64> {
        match read_scalar(doc, p, k)? {
            ScalarValue::Int(x) => Some(x),
            ScalarValue::Uint(x) => i64::try_from(x).ok(),
            _ => None,
        }
    }
}
fn recent_id(q: &str) -> String {
    hex::encode(q.trim().to_lowercase())
}
macro_rules! names{($a:ident,$b:ident,$t:ty,{$($v:ident=>$s:literal),+})=>{fn $a(x:$t)->&'static str{match x{$(<$t>::$v=>$s),+}}fn $b(x:&str)->Option<$t>{match x{$($s=>Some(<$t>::$v)),+,_=>None}}};}
names!(category_name,category,Category,{Landmark=>"landmark",Cafe=>"cafe",Restaurant=>"restaurant",Bar=>"bar",Museum=>"museum",Park=>"park",Transit=>"transit",Hotel=>"hotel",Shop=>"shop",Hospital=>"hospital",Other=>"other"});
names!(travel_name,travel,TravelMode,{Car=>"car",Transit=>"transit",Bike=>"bike",Foot=>"foot"});
fn time(x: &str) -> Option<TransitTimeMode> {
    match x {
        "depart" => Some(TransitTimeMode::Depart),
        "arrive" => Some(TransitTimeMode::Arrive),
        _ => None,
    }
}
#[cfg(test)]
fn time_name(x: TransitTimeMode) -> &'static str {
    match x {
        TransitTimeMode::Depart => "depart",
        TransitTimeMode::Arrive => "arrive",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;
    #[test]
    fn schema_excludes_window_local_state() {
        // Phase `typed_schema_excludes_window_local_state`.
        {
            let mut d = AutoCommit::new();
            let mut m = sample::default_model();
            m.selected = Some("marienplatz".into());
            m.panel = crate::model::Panel::Directions;
            m.transit_choice = 1;
            MapsDoc::save(&mut d, &m);
            let out = MapsDoc::load(&d).unwrap();
            assert_eq!(
                out.route.as_ref().map(|route| route.mode),
                m.route.as_ref().map(|route| route.mode)
            );
            assert!(
                (out.route.as_ref().map_or(0.0, |route| route.distance_m)
                    - m.route.as_ref().map_or(0.0, |route| route.distance_m))
                .abs()
                    < f64::EPSILON
            );
            assert_eq!(out.transit.len(), m.transit.len());
            assert_eq!(
                out.transit
                    .iter()
                    .map(|journey| (&journey.dep, &journey.arr, journey.duration_min))
                    .collect::<Vec<_>>(),
                m.transit
                    .iter()
                    .map(|journey| (&journey.dep, &journey.arr, journey.duration_min))
                    .collect::<Vec<_>>()
            );
            assert!(out.results.is_empty());
            assert!(out.selected.is_none());
            assert_eq!(out.panel, crate::model::Panel::Explore);
            assert_eq!(out.transit_choice, 0);
            assert!(d.get(&ROOT, "maps").unwrap().is_none())
        }
    }
}
