//! Source-owned login groups; presentation never opens a native profile path.
use super::*;
#[cfg(test)]
use ui::widget::Row;
use ui::widget::{IconWidget, Image, Section, SectionHeader};

/// A native app's default profile often repeats the app name. Browser
/// profiles and genuinely distinct native profiles still identify the source.
fn source_name(application: &str, label: &str, native: bool) -> String {
    let application = super::super::review_source_label(application);
    let label = super::super::review_source_label(label);
    if native && label == format!("{application} · {application}") {
        application
    } else {
        label
    }
}

/// Registry application identity only, never a destination service or profile
/// label/path. Native and browser identities cannot impersonate one another.
fn source_icon(application: &str, native: bool) -> Option<&'static [u8]> {
    match (native, application) {
        (true, "Slack" | "com.slack.Slack") => Some(include_bytes!("icons/slack.svg")),
        (true, "Discord" | "com.discordapp.Discord") => Some(include_bytes!("icons/discord.svg")),
        (false, "Google Chrome") => Some(include_bytes!("icons/chrome.svg")),
        (false, "Chromium") => Some(include_bytes!("icons/chromium.svg")),
        (false, "Firefox") => Some(include_bytes!("icons/firefox.svg")),
        (false, "Brave") => Some(include_bytes!("icons/brave.svg")),
        _ => None,
    }
}

/// Presentation eligibility is not authority: mutations must still validate the source.
fn selectable(source: &CookieProfile, presence: Presence) -> bool {
    presence != Presence::NeedsAccess || source.supports_encrypted_cookie_capture()
}

impl Selection {
    pub(in crate::setup::journey) fn expand_group(
        &mut self,
        id: &str,
        expanded: bool,
    ) -> Result<(), &'static str> {
        if !self.detected.iter().any(|(source, _, _)| source == id) {
            return Err("installer-service-source-changed");
        }
        if expanded {
            self.collapsed.remove(id);
        } else {
            self.collapsed.insert(id.into());
        }
        Ok(())
    }
    pub(in crate::setup::journey) fn set_group(
        &mut self,
        id: &str,
        included: bool,
    ) -> Result<bool, &'static str> {
        let source = self
            .sources
            .iter()
            .find(|source| source.id == id)
            .ok_or("installer-service-source-changed")?;
        source
            .validate()
            .map_err(|_| "installer-service-source-changed")?;
        if !self.detected.iter().any(|(source, _, _)| source == id) {
            return Err("installer-service-source-changed");
        }
        let services = self
            .detected
            .iter()
            .filter(|(source_id, _, presence)| source_id == id && selectable(source, *presence))
            .map(|(_, service, _)| *service)
            .collect::<Vec<_>>();
        let previous = self.included.clone();
        let decisions = self.decided.clone();
        let mut changed = false;
        for service in services {
            match self.set(id, service, included) {
                Ok(value) => changed |= value,
                Err(error) => {
                    self.included = previous;
                    self.decided = decisions;
                    return Err(error);
                }
            }
        }
        Ok(changed)
    }

    pub(in crate::setup::journey) fn logins_view(
        &self,
        enabled: bool,
        bindings: impl Fn(String, Service, bool) + 'static + Clone,
        groups: impl Fn(String, bool) + 'static + Clone,
        expand: impl Fn(String, bool) + 'static + Clone,
    ) -> Element {
        if !enabled {
            return Box::new(Text::new(&super::super::t(
                "installer-service-sessions-accounts-excluded",
            )));
        }
        let mut body = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(Rhythm::Group);
        for source in &self.sources {
            let detected = self
                .detected
                .iter()
                .filter(|(id, _, _)| id == &source.id)
                .collect::<Vec<_>>();
            if detected.is_empty() {
                continue;
            }
            let count = detected
                .iter()
                .filter(|(_, _, presence)| selectable(source, *presence))
                .count();
            let selected = detected
                .iter()
                .filter(|(_, service, presence)| {
                    selectable(source, *presence)
                        && self.included.contains(&(source.id.clone(), *service))
                })
                .count();
            let summary = super::super::t_args(
                if count == detected.len() {
                    "installer-logins-group-count"
                } else {
                    "installer-logins-group-selectable-count"
                },
                &[("selected", selected.into()), ("total", count.into())],
            );
            let mut items: Vec<Element> = Vec::new();
            let chosen = ui::LocalState::new(count > 0 && selected == count);
            let id = source.id.clone();
            let change = groups.clone();
            let selection: Element = Box::new(
                Checkbox::from_state(&chosen)
                    .indeterminate(selected > 0 && selected < count)
                    .with_enabled(count > 0)
                    .with_id(&format!("installer.logins.group.{}", source.id))
                    .with_label(if count == 0 {
                        super::super::t("installer-logins-group-unselectable")
                    } else {
                        super::super::t_args(
                            "installer-logins-select-group",
                            &[("count", summary.clone().into())],
                        )
                    })
                    .on_change(move |included| change(id.clone(), included)),
            );
            for (_, service, presence) in detected {
                let label = super::super::t(match service {
                    Service::Slack => "installer-service-slack-label",
                    Service::Discord => "installer-service-discord-label",
                });
                if *presence == Presence::NeedsAccess {
                    items.push(Box::new(Text::new(&super::super::t_args(
                        if source.supports_encrypted_cookie_capture() {
                            "installer-service-source-needs-access"
                        } else {
                            "installer-service-source-reconnect-required"
                        },
                        &[("source", label.clone().into())],
                    ))));
                }
                if !selectable(source, *presence) {
                    continue;
                }
                let chosen =
                    ui::LocalState::new(self.included.contains(&(source.id.clone(), *service)));
                let id = source.id.clone();
                let service = *service;
                let change = bindings.clone();
                items.push(Box::new(
                    Checkbox::from_state(&chosen)
                        .with_id(&format!(
                            "installer.session.{}.{}",
                            service.as_str(),
                            source.id
                        ))
                        .with_label(label)
                        .on_change(move |included| change(id.clone(), service, included)),
                ));
            }
            // Only registry-owned product names choose bundled artwork. Unknown
            // sources remain honest: no guessed browser, path, or account name.
            let icon = source_icon(&source.application, source.native_application);
            let leading: Element =
                if let Some(image) = icon.and_then(|bytes| Image::from_svg(bytes, 32, 32).ok()) {
                    Box::new(
                        image
                            .with_icon_size(ui::widget::IconSize::Small)
                            .with_alt(source.application.clone()),
                    )
                } else {
                    Box::new(IconWidget::new(if source.native_application {
                        ui::icons::Icon::LayoutGrid
                    } else {
                        ui::icons::Icon::Globe
                    }))
                };
            let section = Section::new(items).with_header(
                SectionHeader::group(source_name(
                    &source.application,
                    &source.label,
                    source.native_application,
                ))
                .leading(leading),
            );
            let section = if self.collapsed.contains(&source.id) {
                section.collapsed()
            } else {
                section.collapsible()
            };
            let id = source.id.clone();
            let toggle = expand.clone();
            // Keep group selection and its one count visible when collapsed.
            // The shared Section owns disclosure; the shared Checkbox owns selection.
            body = body.push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(Rhythm::Related)
                    .push(selection)
                    .push(Box::new(
                        section.on_toggle(move |expanded| toggle(id.clone(), expanded)),
                    )),
            ));
        }
        Box::new(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_names_preserve_profile_boundaries_without_repeating_native_defaults() {
        assert_eq!(source_name("Slack", "Slack · Slack", true), "Slack");
        assert_eq!(source_name("Slack", "Slack · Work", true), "Slack · Work");
        assert_eq!(
            source_name("Firefox", "Firefox · Personal", false),
            "Firefox · Personal"
        );
        assert_eq!(
            source_name("Unknown", "Unknown · Work", true),
            "Unknown · Work"
        );
        assert_eq!(
            source_name("Firefox", "Firefox · Firefox", false),
            "Firefox · Firefox"
        );
    }

    #[test]
    fn all_bundled_source_logos_load_as_vector_images() {
        let mut row = Row::new();
        for (application, native) in [
            ("Google Chrome", false),
            ("Chromium", false),
            ("Firefox", false),
            ("Brave", false),
            ("Slack", true),
            ("Discord", true),
        ] {
            let bytes = source_icon(application, native).unwrap();
            let text = std::str::from_utf8(bytes).expect("artwork is text, not a binary asset");
            assert!(text.contains("<svg"));
            assert!(!text.contains('\r'), "artwork uses LF line endings");
            for forbidden in ["<image", "data:", "<script", "<!ENTITY", "<!DOCTYPE"] {
                assert!(!text.contains(forbidden), "{application}: {forbidden}");
            }
            let image = Image::from_svg(bytes, 32, 32)
                .expect("every included source logo loads through the SDK vector API")
                .with_icon_size(ui::widget::IconSize::Small)
                .with_alt(application);
            assert_eq!(image.intrinsic_size(), (32, 32));
            assert_eq!(image.alt(), Some(application));
            row = row.push(Box::new(image));
        }
        let theme = ui::Theme::dark().with_ui_scale(2.0);
        let edge = f64::from(theme.tokens.metrics.icon_sm);
        let harness = ui::headless::FrameHarness::new_with_theme(
            ui::State::new(),
            Box::new(row),
            (800, 200),
            theme,
        );
        let images = harness.root_widget().children();
        assert_eq!(images.len(), 6);
        for image in images {
            let rect = image.get_rect();
            assert_eq!(
                rect.width(),
                edge,
                "artwork resolves the scaled icon token once"
            );
            assert_eq!(rect.height(), edge);
        }
    }

    #[test]
    fn native_artwork_uses_exact_source_identity_not_destination_or_path() {
        for aliases in [
            ["Slack", "com.slack.Slack"],
            ["Discord", "com.discordapp.Discord"],
        ] {
            let expected = source_icon(aliases[0], true).unwrap();
            Image::from_svg(expected, 64, 64).expect("bundled native logo decodes");
            for alias in aliases {
                assert_eq!(source_icon(alias, true), Some(expected));
                assert!(
                    source_icon(alias, false).is_none(),
                    "service is not a browser origin"
                );
            }
        }
        assert_ne!(source_icon("Slack", true), source_icon("Discord", true));
        for browser in ["Google Chrome", "Chromium", "Firefox", "Brave"] {
            Image::from_svg(source_icon(browser, false).unwrap(), 64, 64).unwrap();
            assert!(source_icon(browser, true).is_none());
        }
        for unknown in [
            "Unknown",
            "slack",
            "Discord Canary",
            "/home/owner/Slack",
            "Firefox · Slack",
            "Slack · personal",
        ] {
            for native in [true, false] {
                assert!(source_icon(unknown, native).is_none());
            }
        }
    }
}
