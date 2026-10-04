//! Native, field-addressable Automerge schema for firewall configuration.
//!
//! Data lives at `firewall.zones[zone].rules[rule-id]`. Rules have independent
//! fields, an Automerge order list, and a separate delete-wins tombstone map.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_str, read_str_or_text};

use crate::model::{Action, FirewallConfig, Matcher, Proto, Rule, RuleId, Target, Zone, ZoneKind};

const ROOT_KEY: &str = "firewall";
pub struct FirewallDoc;

fn get_map(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}
fn get_list(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::List), id))) => Some(id),
        _ => None,
    }
}
fn get_text(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Text), id))) => Some(id),
        _ => None,
    }
}
fn map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    get_map(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Map).ok())
}
fn text(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
    let Some(id) =
        get_text(doc, parent, key).or_else(|| doc.put_object(parent, key, ObjType::Text).ok())
    else {
        return;
    };
    ui::doc::splice_text_diffed(doc, &id, value);
}
fn target_name(value: Target) -> &'static str {
    match value {
        Target::Default => "default",
        Target::Accept => "accept",
        Target::Reject => "reject",
        Target::Drop => "drop",
    }
}
fn target(value: &str) -> Option<Target> {
    Some(match value {
        "default" => Target::Default,
        "accept" => Target::Accept,
        "reject" => Target::Reject,
        "drop" => Target::Drop,
        _ => return None,
    })
}
fn action_name(value: Action) -> &'static str {
    match value {
        Action::Allow => "allow",
        Action::Deny => "deny",
    }
}
fn action(value: &str) -> Option<Action> {
    Some(match value {
        "allow" => Action::Allow,
        "deny" => Action::Deny,
        _ => return None,
    })
}
fn proto(value: &str) -> Option<Proto> {
    Some(match value {
        "tcp" => Proto::Tcp,
        "udp" => Proto::Udp,
        _ => return None,
    })
}

fn write_matcher(doc: &mut AutoCommit, row: &ObjId, matcher: &Matcher) {
    match matcher {
        Matcher::Service(service) => {
            text(doc, row, "matcher_kind", "service");
            text(doc, row, "service", service);
        }
        Matcher::Port { port, proto } => {
            text(doc, row, "matcher_kind", "port");
            text(doc, row, "port", port);
            text(doc, row, "proto", proto.label());
        }
    }
}
fn write_rule(doc: &mut AutoCommit, row: &ObjId, rule: &Rule) {
    write_matcher(doc, row, &rule.matcher);
    text(doc, row, "action", action_name(rule.action));
    text(doc, row, "source", &rule.source);
    let _ = doc.put(row, "enabled", rule.enabled);
}
fn read_rule(doc: &AutoCommit, row: &ObjId, id: RuleId) -> Option<Rule> {
    let matcher = match read_str_or_text(doc, row, "matcher_kind")?.as_str() {
        "service" => Matcher::Service(read_str_or_text(doc, row, "service")?),
        "port" => Matcher::Port {
            port: read_str_or_text(doc, row, "port")?,
            proto: proto(&read_str_or_text(doc, row, "proto")?)?,
        },
        _ => return None,
    };
    Some(Rule {
        id,
        matcher,
        action: action(&read_str_or_text(doc, row, "action")?)?,
        source: read_str_or_text(doc, row, "source")?,
        enabled: read_bool(doc, row, "enabled")?,
    })
}

impl FirewallDoc {
    fn has_native(doc: &AutoCommit) -> bool {
        get_map(doc, &ROOT, ROOT_KEY).is_some()
    }
    pub fn load(doc: &AutoCommit) -> Option<FirewallConfig> {
        let root = get_map(doc, &ROOT, ROOT_KEY)?;
        Self::read_native(doc, &root)
    }
    fn read_native(doc: &AutoCommit, root: &ObjId) -> Option<FirewallConfig> {
        let enabled = read_bool(doc, root, "enabled")?;
        let panic_mode = read_bool(doc, root, "panic_mode")?;
        let default_zone = ZoneKind::from_name(&read_str_or_text(doc, root, "default_zone")?)?;
        let zones = get_map(doc, root, "zones")?;
        let mut result = Vec::new();
        for kind in ZoneKind::ALL {
            let Some(zone) = get_map(doc, &zones, kind.name()) else {
                continue;
            };
            let Some(target) = read_str_or_text(doc, &zone, "target").and_then(|v| target(&v))
            else {
                continue;
            };
            let interfaces = get_map(doc, &zone, "interfaces")
                .map(|m| {
                    doc.keys(&m)
                        .filter(|id| read_bool(doc, &m, id) == Some(true))
                        .collect()
                })
                .unwrap_or_default();
            let services = get_map(doc, &zone, "services")
                .map(|m| {
                    doc.keys(&m)
                        .filter(|id| read_bool(doc, &m, id) == Some(true))
                        .collect()
                })
                .unwrap_or_default();
            result.push(Zone {
                kind,
                target,
                interfaces,
                services,
                rules: Self::rules(doc, &zone),
                masquerade: read_bool(doc, &zone, "masquerade").unwrap_or(false),
            });
        }
        Some(FirewallConfig {
            enabled,
            panic_mode,
            default_zone,
            zones: result,
        })
    }
    fn rules(doc: &AutoCommit, zone: &ObjId) -> Vec<Rule> {
        let Some(rows) = get_map(doc, zone, "rules") else {
            return Vec::new();
        };
        let tombs = get_map(doc, zone, "rule_tombstones");
        let mut remaining = std::collections::BTreeMap::new();
        for key in doc.keys(&rows) {
            if tombs
                .as_ref()
                .is_some_and(|m| read_bool(doc, m, &key) == Some(true))
            {
                continue;
            }
            let (Ok(id), Some(row)) = (key.parse::<RuleId>(), get_map(doc, &rows, &key)) else {
                continue;
            };
            if let Some(rule) = read_rule(doc, &row, id) {
                remaining.insert(key, rule);
            }
        }
        let mut ordered = Vec::new();
        if let Some(order) = get_list(doc, zone, "rule_order") {
            for index in 0..doc.length(&order) {
                if let Some(key) = read_str(doc, &order, index)
                    && let Some(rule) = remaining.remove(&key)
                {
                    ordered.push(rule);
                }
            }
        }
        ordered.extend(remaining.into_values());
        ordered
    }
    /// Write a whole configuration. Interactive actions use the narrow
    /// methods.
    pub fn store(doc: &mut AutoCommit, cfg: &FirewallConfig) {
        let Some(root) = map(doc, &ROOT, ROOT_KEY) else {
            return;
        };
        let _ = doc.put(&root, "enabled", cfg.enabled);
        let _ = doc.put(&root, "panic_mode", cfg.panic_mode);
        text(doc, &root, "default_zone", cfg.default_zone.name());
        let Some(zones) = map(doc, &root, "zones") else {
            return;
        };
        for zone in &cfg.zones {
            let Some(row) = map(doc, &zones, zone.kind.name()) else {
                continue;
            };
            text(doc, &row, "target", target_name(zone.target));
            let _ = doc.put(&row, "masquerade", zone.masquerade);
            if let Some(services) = map(doc, &row, "services") {
                for service in &zone.services {
                    let _ = doc.put(&services, service, true);
                }
            }
            if let Some(interfaces) = map(doc, &row, "interfaces") {
                for interface in &zone.interfaces {
                    let _ = doc.put(&interfaces, interface, true);
                }
            }
            for rule in &zone.rules {
                Self::add_rule(doc, zone.kind, rule);
            }
        }
    }
    /// Take in a configuration firewalld reported: written whole into an
    /// empty document, reconciled into an existing one.
    ///
    /// Unlike `store`, reconciling makes absence explicit: stale
    /// interfaces/services become false membership and stale rules receive
    /// delete-wins tombstones. A concurrent user deletion still wins over a
    /// report that carries the same id; an external snapshot never
    /// resurrects a tombstoned rule.
    pub fn import(doc: &mut AutoCommit, cfg: &FirewallConfig) {
        if !Self::has_native(doc) {
            Self::store(doc, cfg);
            return;
        }
        let Some(root) = map(doc, &ROOT, ROOT_KEY) else {
            return;
        };
        let _ = doc.put(&root, "enabled", cfg.enabled);
        let _ = doc.put(&root, "panic_mode", cfg.panic_mode);
        text(doc, &root, "default_zone", cfg.default_zone.name());
        let Some(zones) = map(doc, &root, "zones") else {
            return;
        };
        for incoming in &cfg.zones {
            let Some(zone) = map(doc, &zones, incoming.kind.name()) else {
                continue;
            };
            text(doc, &zone, "target", target_name(incoming.target));
            let _ = doc.put(&zone, "masquerade", incoming.masquerade);
            Self::reconcile_membership(doc, &zone, "interfaces", &incoming.interfaces);
            Self::reconcile_membership(doc, &zone, "services", &incoming.services);
            Self::reconcile_rules(doc, incoming.kind, &incoming.rules);
        }
    }
    fn reconcile_membership(doc: &mut AutoCommit, zone: &ObjId, field: &str, incoming: &[String]) {
        let Some(members) = map(doc, zone, field) else {
            return;
        };
        let desired: std::collections::BTreeSet<_> = incoming.iter().map(String::as_str).collect();
        let existing: Vec<_> = doc.keys(&members).collect();
        for id in existing {
            if !desired.contains(id.as_str()) {
                let _ = doc.put(&members, &id, false);
            }
        }
        for id in desired {
            let _ = doc.put(&members, id, true);
        }
    }
    fn reconcile_rules(doc: &mut AutoCommit, kind: ZoneKind, incoming: &[Rule]) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        let Some(rows) = map(doc, &zone, "rules") else {
            return;
        };
        let desired: std::collections::BTreeSet<_> =
            incoming.iter().map(|rule| rule.id.to_string()).collect();
        let existing: Vec<_> = doc.keys(&rows).collect();
        for id in existing {
            if !desired.contains(&id)
                && let Ok(rule_id) = id.parse()
            {
                Self::delete_rule(doc, kind, rule_id);
            }
        }
        for rule in incoming {
            if Self::tombstoned(doc, &zone, rule.id) {
                continue;
            }
            let key = rule.id.to_string();
            if let Some(row) = get_map(doc, &rows, &key) {
                write_rule(doc, &row, rule);
            } else {
                Self::add_rule(doc, kind, rule);
            }
        }
    }
    fn zone(doc: &AutoCommit, kind: ZoneKind) -> Option<ObjId> {
        get_map(doc, &ROOT, ROOT_KEY)
            .and_then(|root| get_map(doc, &root, "zones"))
            .and_then(|zones| get_map(doc, &zones, kind.name()))
    }
    fn tombstoned(doc: &AutoCommit, zone: &ObjId, id: RuleId) -> bool {
        get_map(doc, zone, "rule_tombstones")
            .is_some_and(|m| read_bool(doc, &m, id.to_string()) == Some(true))
    }
    pub fn add_rule(doc: &mut AutoCommit, kind: ZoneKind, rule: &Rule) {
        let Some(root) = map(doc, &ROOT, ROOT_KEY) else {
            return;
        };
        let Some(zones) = map(doc, &root, "zones") else {
            return;
        };
        let Some(zone) = map(doc, &zones, kind.name()) else {
            return;
        };
        if Self::tombstoned(doc, &zone, rule.id) {
            return;
        }
        let Some(rows) = map(doc, &zone, "rules") else {
            return;
        };
        let key = rule.id.to_string();
        let row = match get_map(doc, &rows, &key) {
            Some(row) => row,
            None => match doc.put_object(&rows, &key, ObjType::Map) {
                Ok(row) => row,
                Err(_) => return,
            },
        };
        if doc.keys(&row).next().is_none() {
            write_rule(doc, &row, rule);
        }
        let order = match get_list(doc, &zone, "rule_order") {
            Some(list) => list,
            None => match doc.put_object(&zone, "rule_order", ObjType::List) {
                Ok(list) => list,
                Err(_) => return,
            },
        };
        if !(0..doc.length(&order))
            .any(|i| read_str(doc, &order, i).as_deref() == Some(key.as_str()))
        {
            let _ = doc.insert(&order, doc.length(&order), key);
        }
    }
    /// Applies fields independently, so two changes to different rule fields
    /// merge rather than replacing a row. Tombstones make delete win.
    pub fn edit_rule(doc: &mut AutoCommit, kind: ZoneKind, rule: &Rule) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        if Self::tombstoned(doc, &zone, rule.id) {
            return;
        }
        let Some(rows) = get_map(doc, &zone, "rules") else {
            return;
        };
        let Some(row) = get_map(doc, &rows, &rule.id.to_string()) else {
            return;
        };
        let Some(current) = read_rule(doc, &row, rule.id) else {
            return;
        };
        if current.source != rule.source {
            text(doc, &row, "source", &rule.source);
        }
        if current.enabled != rule.enabled {
            let _ = doc.put(&row, "enabled", rule.enabled);
        }
        if current.action != rule.action {
            text(doc, &row, "action", action_name(rule.action));
        }
        if current.matcher != rule.matcher {
            write_matcher(doc, &row, &rule.matcher);
        }
    }
    pub fn set_rule_enabled(doc: &mut AutoCommit, kind: ZoneKind, id: RuleId, on: bool) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        if Self::tombstoned(doc, &zone, id) {
            return;
        }
        if let Some(rows) = get_map(doc, &zone, "rules")
            && let Some(row) = get_map(doc, &rows, &id.to_string())
        {
            let _ = doc.put(&row, "enabled", on);
        }
    }
    /// Narrow source patch used by the editable rule control. It intentionally
    /// cannot carry a stale value for another rule field.
    pub fn set_rule_source(doc: &mut AutoCommit, kind: ZoneKind, id: RuleId, source: &str) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        if Self::tombstoned(doc, &zone, id) {
            return;
        }
        if let Some(rows) = get_map(doc, &zone, "rules")
            && let Some(row) = get_map(doc, &rows, &id.to_string())
        {
            text(doc, &row, "source", source);
        }
    }
    pub fn delete_rule(doc: &mut AutoCommit, kind: ZoneKind, id: RuleId) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        let Some(rows) = get_map(doc, &zone, "rules") else {
            return;
        };
        if get_map(doc, &rows, &id.to_string()).is_none() {
            return;
        }
        if let Some(tombs) = map(doc, &zone, "rule_tombstones") {
            let _ = doc.put(&tombs, id.to_string(), true);
        }
    }
    pub fn set_service(doc: &mut AutoCommit, kind: ZoneKind, service: &str, on: bool) {
        if let Some(zone) = Self::zone(doc, kind)
            && let Some(services) = map(doc, &zone, "services")
        {
            let _ = doc.put(&services, service, on);
        }
    }
    pub fn set_zone_target(doc: &mut AutoCommit, kind: ZoneKind, value: Target) {
        if let Some(zone) = Self::zone(doc, kind) {
            text(doc, &zone, "target", target_name(value));
        }
    }
    pub fn set_default_zone(doc: &mut AutoCommit, kind: ZoneKind) {
        if let Some(root) = map(doc, &ROOT, ROOT_KEY) {
            text(doc, &root, "default_zone", kind.name());
        }
    }
    pub fn set_panic(doc: &mut AutoCommit, on: bool) {
        if let Some(root) = map(doc, &ROOT, ROOT_KEY) {
            let _ = doc.put(&root, "panic_mode", on);
        }
    }
    pub fn set_enabled(doc: &mut AutoCommit, on: bool) {
        if let Some(root) = map(doc, &ROOT, ROOT_KEY) {
            let _ = doc.put(&root, "enabled", on);
        }
    }
    pub fn reorder_rule(doc: &mut AutoCommit, kind: ZoneKind, id: RuleId, delta: i32) {
        let Some(zone) = Self::zone(doc, kind) else {
            return;
        };
        let Some(order) = get_list(doc, &zone, "rule_order") else {
            return;
        };
        let key = id.to_string();
        let Some(index) = (0..doc.length(&order))
            .find(|i| read_str(doc, &order, *i).as_deref() == Some(key.as_str()))
        else {
            return;
        };
        let target =
            (index as i32 + delta).clamp(0, doc.length(&order).saturating_sub(1) as i32) as usize;
        if target != index {
            let _ = doc.delete(&order, index);
            let _ = doc.insert(&order, target, key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_config;
    use tilo_automerge::ApplyIncoming;
    /// The native document round-trips and an import removes absent members and rules.
    #[test]
    fn document_round_trips_and_reconciles_probe_state() {
        // --- phase: native_round_trip ---
        {
            let mut d = AutoCommit::new();
            let c = sample_config();
            FirewallDoc::store(&mut d, &c);
            let restored = FirewallDoc::load(&d).unwrap();
            assert_eq!(restored.enabled, c.enabled);
            assert_eq!(restored.panic_mode, c.panic_mode);
            assert_eq!(restored.default_zone, c.default_zone);
            assert_eq!(
                restored,
                c.canonical(),
                "the canonical form is what reads back"
            );
            assert_eq!(
                restored
                    .zones
                    .iter()
                    .map(|zone| zone.rules.len())
                    .sum::<usize>(),
                c.zones.iter().map(|zone| zone.rules.len()).sum::<usize>()
            );
        }

        // --- phase: probe_reconciliation_removes_absent_members_and_rules ---
        {
            let mut doc = AutoCommit::new();
            let before = sample_config();
            FirewallDoc::store(&mut doc, &before);
            let zone_kind = before.default_zone;
            let stale_rule = before.zone(zone_kind).unwrap().rules[0].id;
            let mut probe = before.clone();
            let zone = probe.zone_mut(zone_kind).unwrap();
            zone.services.retain(|service| service != "ssh");
            zone.interfaces.retain(|interface| interface != "eth0");
            zone.rules.retain(|rule| rule.id != stale_rule);

            FirewallDoc::import(&mut doc, &probe);
            let restored = FirewallDoc::load(&doc).unwrap();
            let zone = restored.zone(zone_kind).unwrap();
            assert!(!zone.has_service("ssh"));
            assert!(!zone.interfaces.iter().any(|interface| interface == "eth0"));
            assert!(!zone.rules.iter().any(|rule| rule.id == stale_rule));
        }
    }
    /// Concurrent edits to independent fields, rules and services merge; a delete wins over a stale edit.
    #[allow(
        clippy::many_single_char_names,
        reason = "two replicas, a config, a zone and a rule read best as a, b, c, z and r"
    )]
    #[test]
    fn concurrent_edits_merge_per_field_and_delete_wins() {
        // --- phase: independent_fields_merge ---
        {
            let mut a = AutoCommit::new();
            let c = sample_config();
            FirewallDoc::store(&mut a, &c);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let z = c.default_zone;
            let mut r = c.zone(z).unwrap().rules[0].clone();
            r.source = "10.0.0.0/8".into();
            FirewallDoc::edit_rule(&mut a, z, &r);
            FirewallDoc::set_rule_enabled(&mut b, z, r.id, false);
            a.merge_incoming(&mut b).unwrap();
            let merged = FirewallDoc::load(&a).unwrap();
            let r = merged
                .zone(z)
                .unwrap()
                .rules
                .iter()
                .find(|x| x.id == r.id)
                .unwrap();
            assert_eq!(r.source, "10.0.0.0/8");
            assert!(!r.enabled);
        }

        // --- phase: different_rule_and_service_edits_merge ---
        {
            let mut a = AutoCommit::new();
            let c = sample_config();
            FirewallDoc::store(&mut a, &c);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let z = c.default_zone;
            let ids: Vec<_> = c.zone(z).unwrap().rules.iter().map(|r| r.id).collect();
            FirewallDoc::set_rule_enabled(&mut a, z, ids[0], false);
            FirewallDoc::set_rule_enabled(&mut b, z, ids[1], false);
            FirewallDoc::set_service(&mut a, z, "ssh", true);
            FirewallDoc::set_service(&mut b, z, "https", true);
            a.merge_incoming(&mut b).unwrap();
            let merged = FirewallDoc::load(&a).unwrap();
            let z = merged.zone(z).unwrap();
            let states: Vec<_> = z
                .rules
                .iter()
                .filter(|rule| ids.contains(&rule.id))
                .map(|rule| (rule.id, rule.enabled))
                .collect();
            assert_eq!(
                states.iter().filter(|(_, enabled)| !enabled).count(),
                2,
                "{states:?}"
            );
            assert!(z.has_service("ssh") && z.has_service("https"));
        }

        // --- phase: delete_wins_stale_edit ---
        {
            let mut a = AutoCommit::new();
            let c = sample_config();
            FirewallDoc::store(&mut a, &c);
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let z = c.default_zone;
            let mut r = c.zone(z).unwrap().rules[0].clone();
            r.source = "192.168.1.0/24".into();
            FirewallDoc::delete_rule(&mut a, z, r.id);
            FirewallDoc::edit_rule(&mut b, z, &r);
            a.merge_incoming(&mut b).unwrap();
            assert!(
                !FirewallDoc::load(&a)
                    .unwrap()
                    .zone(z)
                    .unwrap()
                    .rules
                    .iter()
                    .any(|x| x.id == r.id)
            );
        }
    }
}
