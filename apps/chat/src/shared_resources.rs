//! Native resource navigation uses the existing certified catalogue bridge.
//! The message retains only provenance; the host supplies current view metadata.
use tilo_actions::remote::{CataloguePage, CatalogueQuery, RemoteActionMessage, ViewResourceQuery};
use ui::framework::{EffectReply, Effected};

use crate::{app::ChatApp, effects::ChatEffect};

#[derive(Default)]
pub(crate) struct Navigation {
    pending: Option<Pending>,
    requests: Vec<ui::collaboration::CollabRequest>,
}

struct Pending {
    correlation: u64,
    message: String,
    reference: tilo_reference::ResourceRef,
    provider: String,
    open: bool,
    started: ui::time::Instant,
    reply: EffectReply,
}

impl Navigation {
    pub(crate) fn pending(&self) -> bool {
        self.pending.is_some()
    }
}

impl ChatApp {
    pub(crate) fn begin_resource_navigation(
        &mut self,
        effect: &ChatEffect,
        reply: EffectReply,
    ) -> Result<Effected, String> {
        let (message, open) = match effect {
            ChatEffect::CopyResourceLink { message } => (message, false),
            ChatEffect::OpenResourceLink { message } => (message, true),
            _ => return Err("Not a resource navigation effect".into()),
        };
        if self.resource_navigation.pending.is_some() {
            return Err("A resource lookup is already running".into());
        }
        let row = self.message(message).ok_or("Message no longer exists")?;
        let reference = row.reference.clone().ok_or("Message has no resource")?;
        let provider = row
            .source_provider
            .clone()
            .ok_or("This older reference has no source record provider")?;
        let correlation = uuid::Uuid::new_v4().as_u64_pair().0;
        self.resource_navigation
            .requests
            .push(ui::collaboration::CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest {
                    correlation,
                    query: CatalogueQuery {
                        provider: Some(provider.clone()),
                        view_resource: Some(ViewResourceQuery {
                            id: reference.id.clone(),
                            kind: reference.kind.clone(),
                        }),
                        ..CatalogueQuery::default()
                    },
                },
            });
        self.resource_navigation.pending = Some(Pending {
            correlation,
            message: message.clone(),
            reference,
            provider,
            open,
            started: ui::time::now(),
            reply,
        });
        Ok(Effected::Pending)
    }

    pub(crate) fn resource_navigation_requests(&mut self) -> Vec<ui::collaboration::CollabRequest> {
        if self
            .resource_navigation
            .pending
            .as_ref()
            .is_some_and(|pending| {
                pending.reply.is_cancelled()
                    || ui::time::now().saturating_duration_since(pending.started)
                        >= std::time::Duration::from_secs(30)
            })
            && let Some(pending) = self.resource_navigation.pending.take()
        {
            pending
                .reply
                .finish(Err("Resource lookup cancelled or timed out".into()));
            self.resource_navigation.requests.clear();
        }
        std::mem::take(&mut self.resource_navigation.requests)
    }

    pub(crate) fn complete_resource_navigation(&mut self, message: &RemoteActionMessage) {
        let RemoteActionMessage::CatalogueResponse {
            correlation,
            result,
        } = message
        else {
            return;
        };
        if self
            .resource_navigation
            .pending
            .as_ref()
            .is_none_or(|pending| pending.correlation != *correlation)
        {
            return;
        }
        let Some(pending) = self.resource_navigation.pending.take() else {
            return;
        };
        let current = self.message(&pending.message).is_some_and(|row| {
            row.reference.as_ref() == Some(&pending.reference)
                && row.source_provider.as_deref() == Some(pending.provider.as_str())
        });
        let link = if !current
            || pending.reply.is_cancelled()
            || ui::time::now().saturating_duration_since(pending.started)
                >= std::time::Duration::from_secs(30)
        {
            Err("Resource reference changed or lookup cancelled".into())
        } else {
            result
                .as_ref()
                .map_err(|_| "Resource view unavailable".to_owned())
                .and_then(|page| navigation_link(page, &pending.reference, &pending.provider))
        };
        let output = link.and_then(|link| {
            if pending.open {
                ui::widget::try_open_external_url(&link).map_err(|error| error.to_string())?;
            } else {
                ui::clipboard_copy(&link);
            }
            Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"url": link})),
                resources: Vec::new(),
            })
        });
        pending.reply.finish(output);
    }
}

/// Encode the browser's version-one fragment from freshly host-issued metadata.
/// No label, payload, credentials or admission material is embedded.
fn navigation_link(
    page: &CataloguePage,
    reference: &tilo_reference::ResourceRef,
    provider: &str,
) -> Result<String, String> {
    let locator = page
        .locator
        .as_ref()
        .ok_or("Resource host supplied no locator")?;
    if locator.provider != provider
        || !crate::actions::valid_source_provider(provider)
        || locator.host_device.len() != 64
        || !locator
            .host_device
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || locator.room.is_empty()
        || locator.room.len() > 128
        || locator.room.chars().any(char::is_control)
    {
        return Err("Resource host locator does not match".into());
    }
    let origin = url::Url::parse(&locator.origin).map_err(|_| "Invalid resource host origin")?;
    if origin.scheme() != "https" || origin.origin().ascii_serialization() != locator.origin {
        return Err("Resource host requires a canonical HTTPS origin".into());
    }
    let mut destination = url::Url::parse(
        locator
            .browser_page
            .as_deref()
            .ok_or("Resource browser entry is not configured on this host")?,
    )
    .map_err(|_| "Invalid resource browser entry")?;
    if destination.scheme() != "https"
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.query().is_some()
        || destination.fragment().is_some()
    {
        return Err("Resource browser entry requires an HTTPS page without credentials".into());
    }
    let action = page
        .rows
        .iter()
        .filter(|row| {
            row.provider == provider
                && row.resource.id == reference.id
                && row.resource.kind == reference.kind
        })
        .filter_map(|row| row.action.as_ref())
        .filter(|action| {
            action.kind == tilo_actions::OperationKind::View
                && action.address.resource == reference.id
                && action.resource_kind == reference.kind
        })
        .filter(|action| {
            action.view.as_ref().is_some_and(|view| {
                view.open_action == action.address.action
                    && (reference.span.is_none() || view.range.is_some())
            })
        })
        .max_by_key(|action| action.view.as_ref().is_some_and(|view| view.primary))
        .ok_or("No permitted declared resource view")?;
    let mut resource = serde_json::Map::from_iter([
        ("kind".into(), serde_json::json!(reference.kind)),
        ("id".into(), serde_json::json!(reference.id)),
    ]);
    if let Some(span) = reference.span {
        if span.start > span.end
            || u64::try_from(span.end).unwrap_or(u64::MAX) > 9_007_199_254_740_991
        {
            return Err("Invalid resource range".into());
        }
        resource.insert(
            "span".into(),
            serde_json::to_value(span).map_err(|error| error.to_string())?,
        );
    }
    let address = serde_json::json!({"version": 1, "origin": locator.origin,
        "room": locator.room, "hostDevice": locator.host_device.to_ascii_lowercase(),
        "provider": provider, "action": action.address.action, "reference": resource});
    // Percent-encode UTF-8 bytes, matching decodeURIComponent in resource-link.mjs.
    let encoded = serde_json::to_string(&address)
        .map_err(|error| error.to_string())?
        .bytes()
        .fold(String::new(), |mut result, byte| {
            use std::fmt::Write as _;
            let _ = write!(result, "%{byte:02X}");
            result
        });
    if encoded.len() > 23_990 {
        return Err("Resource address is too large".into());
    }
    destination.set_fragment(Some(&format!("resource={encoded}")));
    Ok(destination.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueResource, CatalogueRow, ResourceViewLocator};

    fn page() -> CataloguePage {
        use tilo_actions::{Resource, ResourceViewRangeSpec, ResourceViewSpec};
        use ui::tilo_resource::Effect as _;
        let resource = Resource::new("chat.message:message", "chat.message", "Fixture")
            .with_action(
                ChatEffect::RevealMessage {
                    message: "message".into(),
                }
                .bound(),
            )
            .with_view(
                ResourceViewSpec::new("message", "Message", "reveal-message")
                    .with_primary(true)
                    .with_range(ResourceViewRangeSpec::new("reveal-message-range")),
            );
        let descriptor = tilo_actions::door::survey(&[resource])
            .into_iter()
            .next()
            .unwrap();
        CataloguePage {
            locator: Some(ResourceViewLocator {
                origin: "https://host.example".into(),
                room: "fixture-room".into(),
                host_device: "ab".repeat(32),
                provider: "record:so.tilo.chat:discussion".into(),
                browser_page: Some("https://cloud.example/qos-runtime/resource.html".into()),
            }),
            rows: vec![CatalogueRow {
                provider: "record:so.tilo.chat:discussion".into(),
                app_id: "so.tilo.chat".into(),
                parent_resource: String::new(),
                resource: CatalogueResource {
                    id: "chat.message:message".into(),
                    kind: "chat.message".into(),
                    label: "Fixture".into(),
                    presentation: None,
                },
                action: Some(descriptor),
                has_children: false,
                visibility: None,
            }],
            ..CataloguePage::default()
        }
    }

    fn assert_native_resource_controls(
        app: &mut ui::framework::testing::TestApp<crate::resource::ChatShell>,
    ) {
        for theme in [
            ui::Theme::dark(),
            ui::Theme::light(),
            ui::Theme::high_contrast(),
        ] {
            app.harness().set_theme(theme);
            app.harness().resize((390, 844)).unwrap();
            app.settle();
            assert!(
                app.harness()
                    .a11y_node_named("Copy resource link")
                    .is_some()
            );
            assert!(
                app.harness()
                    .a11y_node_named("Open resource in browser")
                    .is_some()
            );
            let controls: Vec<_> = app
                .surface_mut::<ChatApp>()
                .hits
                .iter()
                .filter(|(_, hit)| {
                    matches!(
                        hit,
                        crate::app::Hit::CopyResourceLink(_) | crate::app::Hit::OpenResourceLink(_)
                    )
                })
                .map(|(rect, _)| *rect)
                .collect();
            assert_eq!(controls.len(), 2);
            assert!(controls.iter().all(|rect| rect.width() > 0.0
                && rect.height() > 0.0
                && rect.x0 >= 0.0
                && rect.x1 <= 390.0));
        }
    }

    #[test]
    fn native_copy_uses_the_public_effect_and_fences_changed_messages_and_cancellation() {
        use crate::resource::ChatShell;
        use ui::framework::testing::TestApp;
        let mut app = TestApp::<ChatShell>::new().unwrap();
        let reference = serde_json::json!({"kind":"chat.message", "id":"chat.message:message", "label":"Fixture"});
        app.call_on("chat.channel:c_general", "share-resource", serde_json::json!({
            "reference":reference, "sourceProvider":"record:so.tilo.chat:discussion", "text":"Discuss resource"
        })).unwrap();
        app.settle();
        let id = app
            .state()
            .messages()
            .find(|row| row.text == "Discuss resource")
            .unwrap()
            .id
            .clone();
        assert_native_resource_controls(&mut app);
        let effect = ChatEffect::CopyResourceLink {
            message: id.clone(),
        };
        let target = format!("chat.message:{id}");
        app.start_effect_on(&target, &effect).unwrap();
        let surface = app.surface_mut::<ChatApp>();
        let correlation = surface
            .resource_navigation
            .pending
            .as_ref()
            .unwrap()
            .correlation;
        surface.complete_resource_navigation(&RemoteActionMessage::CatalogueResponse {
            correlation,
            result: Ok(page()),
        });
        app.settle();
        assert!(
            ui::clipboard_paste()
                .unwrap()
                .starts_with("https://cloud.example/qos-runtime/resource.html#resource=")
        );
        ui::clipboard_copy("sentinel");
        let invocation = app.start_effect_on(&target, &effect).unwrap();
        let correlation = app
            .surface_mut::<ChatApp>()
            .resource_navigation
            .pending
            .as_ref()
            .unwrap()
            .correlation;
        app.cancel_effect(invocation);
        app.surface_mut::<ChatApp>().complete_resource_navigation(
            &RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(page()),
            },
        );
        app.settle();
        assert_eq!(ui::clipboard_paste().as_deref(), Some("sentinel"));
        app.start_effect_on(&target, &effect).unwrap();
        let surface = app.surface_mut::<ChatApp>();
        let correlation = surface
            .resource_navigation
            .pending
            .as_ref()
            .unwrap()
            .correlation;
        // Remove the source through the real message action while its lookup waits.
        app.call_confirmed_on(&target, "delete", serde_json::json!({}))
            .unwrap();
        app.settle();
        app.surface_mut::<ChatApp>().complete_resource_navigation(
            &RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(page()),
            },
        );
        app.settle();
        assert_eq!(ui::clipboard_paste().as_deref(), Some("sentinel"));
        assert!(!app.surface_mut::<ChatApp>().resource_navigation.pending());
    }

    #[test]
    fn native_link_carries_only_the_current_host_locator_and_exact_declared_view() {
        let reference = tilo_reference::ResourceRef::ranged(
            "chat.message",
            "chat.message:message",
            "Never export this label",
            tilo_reference::Span::chars(6, 7),
        );
        let page = page();
        let link = navigation_link(&page, &reference, "record:so.tilo.chat:discussion").unwrap();
        let url = url::Url::parse(&link).unwrap();
        assert_eq!(url.origin().ascii_serialization(), "https://cloud.example");
        assert!(url.query().is_none());
        let raw = url.fragment().unwrap().strip_prefix("resource=").unwrap();
        let decoded = url::form_urlencoded::parse(format!("value={raw}").as_bytes())
            .next()
            .unwrap()
            .1
            .into_owned();
        let address: serde_json::Value = serde_json::from_str(&decoded).unwrap();
        assert_eq!(address["origin"], "https://host.example");
        assert_eq!(address["provider"], "record:so.tilo.chat:discussion");
        assert_eq!(address["action"], "reveal-message");
        assert_eq!(
            address["reference"]["span"],
            serde_json::json!({"start":6,"end":7,"unit":"Chars"})
        );
        assert!(!decoded.contains("Never export this label"));
        // The paired browser fixture parses this actual native-generated link.
        println!("NATIVE_RESOURCE_LINK={link}");
        let mut denied = page.clone();
        denied.locator.as_mut().unwrap().browser_page = None;
        assert!(navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err());
        denied = page.clone();
        denied.rows.clear();
        assert!(navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err());
        denied = page.clone();
        denied.rows[0].resource.kind = "other.kind".into();
        assert!(navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err());
        denied = page.clone();
        denied.rows[0].action.as_mut().unwrap().kind = tilo_actions::OperationKind::Document;
        assert!(navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err());
        denied = page.clone();
        denied.rows[0]
            .action
            .as_mut()
            .unwrap()
            .view
            .as_mut()
            .unwrap()
            .range = None;
        assert!(navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err());
        for unsafe_page in [
            "http://cloud.example/resource.html",
            "https://name:secret@cloud.example/resource.html",
            "https://cloud.example/resource.html?token=secret",
        ] {
            denied = page.clone();
            denied.locator.as_mut().unwrap().browser_page = Some(unsafe_page.into());
            assert!(
                navigation_link(&denied, &reference, "record:so.tilo.chat:discussion").is_err()
            );
        }
    }
}
