//! The firewall's composed view — a plain-language stack of cards built from
//! shared `ui` widgets, with the raw firewalld machinery layered behind an
//! Advanced disclosure.
//!
//! Every builder returns a `Box<dyn Widget>` tree; the host ([`FirewallApp`])
//! rebuilds it whenever the view structure changes. Nothing here paints
//! directly — controls are [`Toggle`], [`Button`], [`ToggleGroup`],
//! [`Dropdown`] and [`TextInput`], so keyboard focus, the shared focus ring,
//! accessibility and theming come for free (HIG §7/§8/§9).

use vello::peniko::Color;

use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Badge, Button, Card, Column, Container, Divider, Dropdown, Element, EmptyState, IconWidget,
    Row, Scrollable, Spacer, Text, TextInput, Toggle, ToggleGroup,
};

use crate::app::{
    k_rule, k_service, k_target, target_label, FirewallApp, Filter, Intent, K_ENABLED, K_FORM_PORT,
    K_FORM_SOURCE, K_PANIC, K_VIEWZONE,
};
use crate::model::{
    service_intent_label, Action, Proto, ServiceCatalog, Target, ZoneKind, COMMON_SERVICE_IDS,
    PRIMARY_PROFILES,
};

// ---- small composition helpers (mirrors settings/widgets.rs) ------------
const CARD_GAP: f32 = 16.0;
const ROW_GAP: f32 = 12.0;

fn dim() -> Color {
    ui::Theme::dark().text_dim
}
fn success() -> Color {
    ui::Theme::dark().tokens.colors.semantic.success
}
fn danger() -> Color {
    ui::Theme::dark().tokens.colors.semantic.danger
}
fn warning() -> Color {
    ui::Theme::dark().tokens.colors.semantic.warning
}
fn info() -> Color {
    ui::Theme::dark().tokens.colors.semantic.info
}
fn accent() -> Color {
    ui::Theme::dark().accent_primary
}

fn vgap(px: f32) -> Element {
    Box::new(Container::new().with_no_padding().with_height(px))
}
fn hgap(px: f32) -> Element {
    Box::new(Container::new().with_no_padding().with_size(px, 1.0))
}
fn caption(text: &str) -> Element {
    Box::new(Text::new(text).with_size(12.0).with_color(dim()))
}
fn title_text(text: &str) -> Element {
    Box::new(Text::new(text).with_size(15.0))
}

/// A flex-growing column of a title line and a dim detail line — the middle of
/// a list row, so the trailing control is pushed to the right edge.
fn label_block(title: &str, detail: &str) -> Element {
    let col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(title_text(title))
        .push(vgap(2.0))
        .push(caption(detail));
    Box::new(Container::new().with_no_padding().with_flex_grow().with_child(Box::new(col)))
}

/// Stack children with `gap` px of air between them (an intrinsic-height column
/// lays out as a block stack that ignores the toolkit gap, so we inject
/// explicit spacers — the same trick the Settings panes use).
fn stack(items: Vec<Element>, gap: f32) -> Element {
    let mut col = Column::new().with_no_padding().with_intrinsic_height();
    let last = items.len().saturating_sub(1);
    for (i, item) in items.into_iter().enumerate() {
        col = col.push(item);
        if i != last {
            col = col.push(vgap(gap));
        }
    }
    Box::new(col)
}

fn icon(ic: Icon, size: f32, color: Color) -> Element {
    Box::new(IconWidget::new(ic, size).with_color(color))
}

fn service_icon(id: &str) -> Icon {
    match id {
        "ssh" => Icon::Terminal,
        "https" => Icon::Shield,
        "http" => Icon::Wifi,
        "samba" | "nfs" => Icon::FolderOpen,
        "mdns" => Icon::Wifi,
        "ipp" => Icon::Package,
        "wireguard" => Icon::Shield,
        "cockpit" => Icon::Settings,
        _ => Icon::EthernetPort,
    }
}

impl FirewallApp {
    /// Build the whole composed view for the current model + view state.
    pub(crate) fn build_composition(&self) -> Element {
        let body: Element = if !self.is_available() {
            self.unavailable_card()
        } else {
            let mut cards = vec![self.status_card(), self.profile_card(), self.services_card()];
            cards.push(self.advanced_disclosure());
            if self.is_advanced_open() {
                cards.extend(self.advanced_cards());
            }
            stack(cards, CARD_GAP)
        };

        // A default-padded column supplies the horizontal gutter; the whole
        // thing scrolls (HIG §6 — the shared Scrollable, not a bespoke one).
        let inner = Column::new()
            .with_intrinsic_height()
            .push(vgap(6.0))
            .push(body);
        let scroll = Scrollable::new_vertical().with_child(Box::new(inner));
        Box::new(Container::new().filling().with_child(Box::new(scroll)))
    }

    // ---- honest unavailable / empty state -------------------------------
    fn unavailable_card(&self) -> Element {
        Box::new(Card::new(Box::new(
            EmptyState::new("Firewall unavailable")
                .with_icon(Icon::Shield)
                .with_message(
                    "The system firewall service (firewalld) isn't running, so there's \
                     nothing to show. Start it to manage what can reach this machine.",
                ),
        )))
    }

    // ---- status hero ----------------------------------------------------
    fn status_card(&self) -> Element {
        let enabled = self.cfg().enabled;
        let panic = self.cfg().panic_mode;
        let active = self.active_zone();

        let (glyph, tint, headline) = if !enabled {
            (Icon::WifiOff, dim(), "Protection off")
        } else if panic {
            (Icon::Flame, danger(), "Locked down")
        } else {
            (Icon::Shield, success(), "Protected")
        };
        let sub = if !enabled {
            "The firewall is off — anything can reach this machine.".to_string()
        } else if panic {
            "Lockdown is on — all traffic in and out is blocked.".to_string()
        } else {
            format!(
                "{} network · {} allowed in",
                active.profile_name(),
                self.allowed_in_count(active)
            )
        };

        // Hero line: big status glyph + headline/sub + the master on/off switch.
        let hero = Row::new()
            .no_wrap()
            .push(icon(glyph, 30.0, tint))
            .push(hgap(14.0))
            .push(label_block_lg(headline, &sub))
            .push(self.master_toggle());

        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(hero))
            .push(vgap(14.0))
            .push(Box::new(Divider::horizontal()))
            .push(vgap(14.0))
            .push(self.panic_row());

        if !self.status_text().is_empty() {
            col = col
                .push(vgap(12.0))
                .push(Box::new(Text::new(self.status_text()).with_size(12.0).with_color(accent())));
        }

        Box::new(Card::new(Box::new(col)))
    }

    fn master_toggle(&self) -> Element {
        Box::new(
            Toggle::new("firewall.enabled", K_ENABLED)
                .with_id("firewall_enabled")
                .with_accessible_name("Firewall protection"),
        )
    }

    fn panic_row(&self) -> Element {
        let row = Row::new()
            .no_wrap()
            .push(icon(Icon::Flame, 18.0, danger()))
            .push(hgap(12.0))
            .push(label_block(
                "Emergency lockdown",
                "Block every connection in and out until you turn it back off.",
            ))
            .push(Box::new(
                Toggle::new("firewall.panic", K_PANIC)
                    .with_id("firewall_panic")
                    .with_accessible_name("Emergency lockdown"),
            ));
        Box::new(row)
    }

    // ---- network trust profile -----------------------------------------
    fn profile_card(&self) -> Element {
        let active = self.active_zone();
        let selected = PRIMARY_PROFILES.iter().position(|z| *z == active).unwrap_or(0);
        let segments: Vec<Segment> =
            PRIMARY_PROFILES.iter().map(|z| Segment::new(z.profile_name())).collect();
        let bus = self.intents();
        let group = ToggleGroup::new(segments, selected).on_change(move |i| {
            if let Some(z) = PRIMARY_PROFILES.get(i) {
                bus.borrow_mut().push(Intent::UseProfile(*z));
            }
        });

        // The tagline of whichever profile is active — reads at a glance.
        let tagline = caption(active.profile_tagline());
        let note: Element = if PRIMARY_PROFILES.contains(&active) {
            Box::new(Container::new().with_no_padding().with_height(0.0))
        } else {
            caption(&format!(
                "This network uses the “{}” profile (choose it under Advanced).",
                active.profile_name()
            ))
        };

        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption("Pick how much to trust the network you're on."))
            .push(vgap(12.0))
            .push(Box::new(group))
            .push(vgap(12.0))
            .push(tagline)
            .push(note);
        Box::new(Card::titled("This network", Box::new(body)))
    }

    // ---- friendly "what's allowed in" -----------------------------------
    fn services_card(&self) -> Element {
        let active = self.active_zone();
        let enabled = self.cfg().enabled;

        let rows: Vec<Element> = COMMON_SERVICE_IDS
            .iter()
            .map(|id| self.service_row(active, id, enabled))
            .collect();

        let intro = caption("Turn on only what you want other devices to reach here.");
        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(intro)
            .push(vgap(ROW_GAP))
            .push(stack(rows, ROW_GAP));
        Box::new(Card::titled("Allowed in", Box::new(body)))
    }

    /// One Allow/Block switch row for a service in `zone`.
    fn service_row(&self, zone: ZoneKind, id: &str, enabled: bool) -> Element {
        let def = ServiceCatalog::get(id);
        let detail = def.map(|d| format!("Port {} · {}", d.ports, d.description)).unwrap_or_default();
        let tid = format!("svc_{}", id);
        let toggle = Toggle::new("firewall.service", &k_service(zone, id))
            .with_id(&tid)
            .with_enabled(enabled)
            .with_accessible_name(&format!("Allow {}", service_intent_label(id)));
        let row = Row::new()
            .no_wrap()
            .push(icon(service_icon(id), 18.0, dim()))
            .push(hgap(12.0))
            .push(label_block(service_intent_label(id), &detail))
            .push(Box::new(toggle));
        Box::new(row)
    }

    // ---- Advanced disclosure -------------------------------------------
    fn advanced_disclosure(&self) -> Element {
        let open = self.is_advanced_open();
        let (label, glyph) = if open {
            ("Hide advanced settings", Icon::ChevronDown)
        } else {
            ("Advanced settings", Icon::ChevronRight)
        };
        let bus = self.intents();
        let btn = Button::new("firewall.advanced", label, None)
            .ghost()
            .with_icon(glyph)
            .on_click(move || bus.borrow_mut().push(Intent::ToggleAdvanced));
        // Left-align the disclosure control on its own line.
        Box::new(Row::new().no_wrap().push(Box::new(btn)).push(Box::new(Spacer::new())))
    }

    fn advanced_cards(&self) -> Vec<Element> {
        let mut out = Vec::new();
        out.push(self.zone_card());
        if self.zone_ref(self.viewed_zone()).is_some() {
            out.push(self.policy_card());
            out.push(self.all_services_card());
            out.push(self.rules_card());
            out.push(self.add_rule_card());
        }
        out.push(self.apply_card());
        out
    }

    // Advanced: raw zone picker (all nine firewalld zones).
    fn zone_card(&self) -> Element {
        let viewed = self.viewed_zone();
        let active = self.active_zone();
        let options: Vec<String> =
            ZoneKind::ALL.iter().map(|z| z.profile_name().to_string()).collect();
        let dropdown = Dropdown::new(options, K_VIEWZONE).with_id("firewall_zone_picker");

        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption(
                "Every firewalld zone is available here. Pick one to inspect or edit it.",
            ))
            .push(vgap(12.0))
            .push(Box::new(dropdown))
            .push(vgap(10.0))
            .push(caption(viewed.profile_tagline()));

        if viewed != active {
            let bus = self.intents();
            let make_active = Button::new("firewall.make_active", "Make this the active zone", None)
                .primary()
                .on_click(move || bus.borrow_mut().push(Intent::MakeActive));
            col = col.push(vgap(12.0)).push(Box::new(make_active));
        } else {
            col = col.push(vgap(8.0)).push(Box::new(
                Badge::pill("Active zone", accent()),
            ));
        }
        Box::new(Card::titled("Firewall zone", Box::new(col)))
    }

    // Advanced: the zone's default policy (target).
    fn policy_card(&self) -> Element {
        let viewed = self.viewed_zone();
        let cur = self.zone_ref(viewed).map(|z| z.target).unwrap_or(Target::Default);
        let options: Vec<String> =
            crate::app::TARGET_LABELS.iter().map(|(l, _)| (*l).to_string()).collect();
        let dropdown = Dropdown::new(options, &k_target(viewed))
            .with_id("firewall_policy_picker")
            .with_placeholder(target_label(cur));
        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption(
                "What happens to traffic no rule matched. Blocking unmatched traffic is safest.",
            ))
            .push(vgap(12.0))
            .push(Box::new(dropdown));
        Box::new(Card::titled("Default policy", Box::new(body)))
    }

    // Advanced: full service catalogue for the viewed zone.
    fn all_services_card(&self) -> Element {
        let viewed = self.viewed_zone();
        let enabled = self.cfg().enabled;
        let rows: Vec<Element> = ServiceCatalog::ALL
            .iter()
            .map(|def| {
                let tid = format!("adv_svc_{}", def.id);
                let toggle = Toggle::new("firewall.service.adv", &k_service(viewed, def.id))
                    .with_id(&tid)
                    .with_enabled(enabled)
                    .with_accessible_name(&format!("Allow {}", def.name));
                let row = Row::new()
                    .no_wrap()
                    .push(icon(service_icon(def.id), 18.0, dim()))
                    .push(hgap(12.0))
                    .push(label_block(def.name, &format!("Port {} · {}", def.ports, def.proto)))
                    .push(Box::new(toggle));
                Box::new(row) as Element
            })
            .collect();
        Box::new(Card::titled("All services", stack(rows, ROW_GAP)))
    }

    // Advanced: the raw rule list with a filter.
    fn rules_card(&self) -> Element {
        let viewed = self.viewed_zone();
        let filter = self.current_filter();
        let segments: Vec<Segment> =
            Filter::ALL.iter().map(|f| Segment::new(f.label())).collect();
        let selected = Filter::ALL.iter().position(|f| *f == filter).unwrap_or(0);
        let bus = self.intents();
        let filter_group = ToggleGroup::new(segments, selected).on_change(move |i| {
            if let Some(f) = Filter::ALL.get(i) {
                bus.borrow_mut().push(Intent::SetFilter(*f));
            }
        });

        let ids = self.filtered_rule_ids(viewed);
        let body: Element = if ids.is_empty() {
            Box::new(
                EmptyState::new("No rules here")
                    .with_icon(Icon::List)
                    .with_message("Custom port rules you add appear here."),
            )
        } else {
            let rows: Vec<Element> =
                ids.iter().filter_map(|id| self.rule_row(viewed, *id)).collect();
            stack(rows, ROW_GAP)
        };

        Box::new(Card::titled_with_action(
            "Rules",
            Box::new(filter_group),
            body,
        ))
    }

    fn rule_row(&self, zone: ZoneKind, id: crate::model::RuleId) -> Option<Element> {
        let z = self.zone_ref(zone)?;
        let rule = z.rules.iter().find(|r| r.id == id)?;
        let (chip_label, chip_tint) = match rule.action {
            Action::Allow => ("Allow", success()),
            Action::Deny => ("Block", danger()),
        };
        let dot_tint = if rule.matcher.is_service() { accent() } else { info() };
        let sub = format!("{} · from {}", rule.matcher.proto_label(), rule.source_label());

        let toggle = Toggle::new("firewall.rule", &k_rule(zone, id))
            .with_id(&format!("rule_{id}"))
            .with_accessible_name("Rule enabled");

        let bus = self.intents();
        let del = Button::new("firewall.rule.delete", "", None)
            .danger()
            .with_icon(Icon::Trash2)
            .with_size(34.0, 34.0)
            .with_accessible_name("Delete rule")
            .tooltip("Delete rule")
            .on_click(move || bus.borrow_mut().push(Intent::DeleteRule(id)));

        let row = Row::new()
            .no_wrap()
            .push(icon(Icon::Disc3, 12.0, dot_tint))
            .push(hgap(12.0))
            .push(label_block(&rule.matcher.label(), &sub))
            .push(Box::new(Badge::pill(chip_label, chip_tint)))
            .push(hgap(12.0))
            .push(Box::new(toggle))
            .push(hgap(10.0))
            .push(Box::new(del));
        Some(Box::new(row))
    }

    // Advanced: add a custom port rule.
    fn add_rule_card(&self) -> Element {
        let port = TextInput::new(vec![])
            .with_id("firewall_form_port")
            .with_on_change_path(K_FORM_PORT)
            .with_placeholder("Port, e.g. 8080 or 8000-8080")
            .with_font_size(14.0);
        let source = TextInput::new(vec![])
            .with_id("firewall_form_source")
            .with_on_change_path(K_FORM_SOURCE)
            .with_placeholder("From anywhere (or a CIDR like 192.168.0.0/16)")
            .with_font_size(14.0);

        // proto tcp/udp
        let bus_p = self.intents();
        let proto_sel = if self.form_proto() == Proto::Tcp { 0 } else { 1 };
        let proto = ToggleGroup::new(
            vec![Segment::new("TCP"), Segment::new("UDP")],
            proto_sel,
        )
        .on_change(move |i| {
            bus_p
                .borrow_mut()
                .push(Intent::SetFormProto(if i == 0 { Proto::Tcp } else { Proto::Udp }));
        });

        // allow/deny
        let bus_a = self.intents();
        let action_sel = if self.form_action() == Action::Allow { 0 } else { 1 };
        let action = ToggleGroup::new(
            vec![Segment::new("Allow"), Segment::new("Block")],
            action_sel,
        )
        .on_change(move |i| {
            bus_a
                .borrow_mut()
                .push(Intent::SetFormAction(if i == 0 { Action::Allow } else { Action::Deny }));
        });

        let bus_add = self.intents();
        let add = Button::new("firewall.form.add", "Add rule", None)
            .primary()
            .with_icon(Icon::Plus)
            .on_click(move || bus_add.borrow_mut().push(Intent::AddRule));

        let controls = Row::new()
            .push(Box::new(port))
            .push(hgap(10.0))
            .push(Box::new(proto))
            .push(hgap(10.0))
            .push(Box::new(action));

        let body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(caption(&format!(
                "Open or block a specific port in {}.",
                self.viewed_zone().profile_name()
            )))
            .push(vgap(12.0))
            .push(Box::new(controls))
            .push(vgap(10.0))
            .push(Box::new(source))
            .push(vgap(12.0))
            .push(Box::new(Row::new().no_wrap().push(Box::new(Spacer::new())).push(Box::new(add))));
        Box::new(Card::titled("Add a rule", Box::new(body)))
    }

    // Advanced: apply staged changes to the live firewall.
    fn apply_card(&self) -> Element {
        let mode = self.backend_mode();
        let (tint, note): (Color, String) = match mode {
            crate::backend::Mode::Live => (
                success(),
                "Applying is armed — changes are pushed to the live firewall.".to_string(),
            ),
            crate::backend::Mode::DryRun => (
                warning(),
                "Changes are staged. The system will ask permission before they go live."
                    .to_string(),
            ),
        };
        let bus = self.intents();
        let apply = Button::new("firewall.apply", "Apply changes", None)
            .primary()
            .with_icon(Icon::Check)
            .on_click(move || bus.borrow_mut().push(Intent::Apply));

        let mode_pill = Badge::pill(
            match mode {
                crate::backend::Mode::Live => "Live",
                crate::backend::Mode::DryRun => "Staged",
            },
            tint,
        );

        let row = Row::new()
            .no_wrap()
            .push(Box::new(mode_pill))
            .push(hgap(12.0))
            .push(Box::new(
                Container::new().with_no_padding().with_flex_grow().with_child(caption(&note)),
            ))
            .push(Box::new(apply));
        Box::new(Card::titled("Apply", Box::new(row)))
    }
}

/// A larger label block for the status hero (bigger title, dim sub).
fn label_block_lg(title: &str, detail: &str) -> Element {
    let col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(Text::new(title).with_size(20.0)))
        .push(vgap(3.0))
        .push(caption(detail));
    Box::new(Container::new().with_no_padding().with_flex_grow().with_child(Box::new(col)))
}
