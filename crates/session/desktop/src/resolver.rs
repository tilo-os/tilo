//! Exact application identity and checked mounting. Hosts supply admitted records/services.
use tilo_desktop_model::{RoomId, Tab};
use ui::application::{
    AppComponent, ApplicationCatalog, ApplicationDefinition, HostContext, LaunchTarget,
    RestoreInput,
};
use ui::launch::LaunchError;
use ui::record::RecordCheckout;
use ui::widget::{AppWidget, TileHost};

pub struct TileRequest<'a> {
    pub room: RoomId,
    pub tab: &'a Tab,
}
#[expect(
    clippy::large_enum_variant,
    reason = "a resolution is mounted immediately; boxing the component would reshape the public variants hosts construct"
)]
pub enum TileResolution {
    Component(AppComponent),
    /// A component shown as an App of its own in the tile: its input,
    /// focus, palette, history and collaboration are its App's, the last
    /// over the collaboration manager the host connected it with.
    App(
        AppComponent,
        Option<ui::collaboration::CollaborationManager>,
    ),
    /// A retained checked owner moved through a hierarchy rebuild.
    Retained(TileHost),
    Hosted(Box<dyn AppWidget>),
    Content(ui::widget::TileContent),
    Unavailable {
        title: String,
        detail: String,
    },
    OpaqueForeign {
        app_id: String,
        reason: String,
    },
    /// Retain failed setup owners, including retryable persistence reports.
    Failed(LaunchError),
    /// The app's record is still being opened off the frame loop (see
    /// [`LaunchError::is_opening`]): its window reserves the place for the
    /// app's arrival, and the tab is resolved again on the next
    /// reconciliation, which the host's storage wake causes.
    Opening,
}
pub trait TileResolver {
    fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution;
    fn ready_to_replace(&self, _tab: &Tab) -> bool {
        true
    }
    fn creation_refusal(&self, _tab: &Tab) -> Option<String> {
        None
    }
    /// The application's own artwork for `tab`, shown in place of its kind's
    /// glyph. Only the host can resolve it (icon theme, window-set icons).
    fn tab_icon(&self, _tab: &Tab) -> Option<ui::icons::AppImage> {
        None
    }
}

/// A checked context is provided by the host; this interface grants no authority.
pub trait ComponentContextSource {
    /// The record checkout for a component tile.
    ///
    /// # Errors
    ///
    /// Returns the launch error when the host cannot provide the record.
    fn checkout(
        &self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
    ) -> Result<RecordCheckout, LaunchError> {
        Ok(crate::records::component_record(
            definition.app_id,
            request.tab.id,
            definition.record_kind.clone(),
        ))
    }

    /// Prepare the checked host context and restore input for a launch.
    ///
    /// # Errors
    ///
    /// Returns the launch error when the host cannot prepare the component.
    fn prepare(
        &mut self,
        definition: &'static ApplicationDefinition,
        request: &TileRequest<'_>,
        target: &LaunchTarget,
    ) -> Result<(HostContext, RestoreInput), LaunchError>;
}
pub struct ApplicationCatalogTileResolver<H> {
    pub catalog: ApplicationCatalog,
    pub host: H,
}
impl<H: ComponentContextSource> TileResolver for ApplicationCatalogTileResolver<H> {
    fn resolve(&mut self, request: TileRequest<'_>) -> TileResolution {
        let Some(id) = tilo_desktop_model::apps::canonical_app_id(&request.tab.tile) else {
            return TileResolution::Unavailable {
                title: request.tab.title.clone(),
                detail: crate::i18n::t("desktop-app-identity-unavailable"),
            };
        };
        let Ok(definition) = self.catalog.resolve(&id) else {
            return if tilo_desktop_model::first_party::is_first_party_app_id(&id) {
                TileResolution::Unavailable {
                    title: request.tab.title.clone(),
                    detail: crate::i18n::t_args(
                        "desktop-app-integration-unavailable",
                        &[("app", id.as_str().into())],
                    ),
                }
            } else {
                TileResolution::OpaqueForeign {
                    app_id: id,
                    reason: crate::i18n::t("desktop-app-no-adapter"),
                }
            };
        };
        let checkout = match self.host.checkout(definition, &request) {
            Ok(checkout) => checkout,
            Err(error) => return TileResolution::Failed(error),
        };
        let target = LaunchTarget::Record {
            checkout,
            reveal: None,
        };
        match self
            .host
            .prepare(definition, &request, &target)
            .and_then(|(context, restore)| definition.instantiate(context, target, restore))
        {
            Ok(component) => TileResolution::Component(component),
            Err(error) => TileResolution::Failed(error),
        }
    }
}
impl TileResolution {
    pub(crate) fn mount(
        self,
        failures: &mut Vec<LaunchError>,
        wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    ) -> TileHost {
        match self {
            Self::Retained(host) => host,
            Self::Component(component) => {
                let mounted = TileHost::mount_component(component, move |_| wake());
                match mounted {
                    Ok(host) => host,
                    Err(error) => {
                        let detail = error.to_string();
                        failures.push(error);
                        TileHost::new(ui::widget::TileContent::Notice {
                            title: crate::i18n::t("desktop-app-unavailable"),
                            detail,
                        })
                    }
                }
            }
            Self::App(component, collaboration) => {
                // The portable toolkit has no App runner: there a component
                // is its own tile.
                #[cfg(not(target_arch = "wasm32"))]
                let mounted = app_tile(component, collaboration, move |_| wake())
                    .map(|app| TileHost::new(ui::widget::TileContent::App(app)));
                #[cfg(target_arch = "wasm32")]
                let mounted = {
                    let _ = collaboration;
                    TileHost::mount_component(component, move |_| wake())
                };
                match mounted {
                    Ok(host) => host,
                    Err(error) => {
                        let detail = error.to_string();
                        failures.push(error);
                        TileHost::new(ui::widget::TileContent::Notice {
                            title: crate::i18n::t("desktop-app-unavailable"),
                            detail,
                        })
                    }
                }
            }
            Self::Hosted(app) => TileHost::new(ui::widget::TileContent::App(app)),
            Self::Content(content) => TileHost::new(content),
            Self::Unavailable { title, detail } => {
                TileHost::new(ui::widget::TileContent::Notice { title, detail })
            }
            Self::OpaqueForeign { app_id, reason } => {
                TileHost::new(ui::widget::TileContent::Notice {
                    title: app_id,
                    detail: reason,
                })
            }
            Self::Opening => TileHost::new(ui::widget::TileContent::Arriving),
            Self::Failed(error) => {
                let detail = error.to_string();
                failures.push(error);
                TileHost::new(ui::widget::TileContent::Notice {
                    title: crate::i18n::t("desktop-app-unavailable"),
                    detail,
                })
            }
        }
    }
}
/// `component` as the app a tile shows: an App of its own, with the input,
/// focus, palette, history and collaboration it has in a window, drawn in
/// the tile, collaborating over `collaboration` when the host connected it.
/// `wake` is told when its background work wants a frame.
///
/// # Errors
/// The app could not be built around the component.
#[cfg(not(target_arch = "wasm32"))]
pub fn app_tile(
    component: ui::application::AppComponent,
    collaboration: Option<ui::collaboration::CollaborationManager>,
    wake: impl Fn(ui::WakeEvent) + Send + Sync + 'static,
) -> Result<Box<dyn AppWidget>, LaunchError> {
    let inputs = ui::application::ComponentRunnerInputs::from_native_process();
    // A private window has no collaboration route; its actions still reach
    // this device's own automation, and nothing beyond it.
    let device_local = component.is_private();
    let app = match collaboration {
        Some(collaboration) => {
            ui::App::from_component_with_collaboration(component, inputs, collaboration)?
        }
        None => ui::App::from_component(component, inputs)?,
    };
    let app = ui::app::EmbeddedApp::new(app, wake)?;
    Ok(Box::new(if device_local {
        app.with_device_local_actions()
    } else {
        app
    }))
}
