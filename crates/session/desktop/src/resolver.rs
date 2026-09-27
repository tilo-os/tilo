//! Exact application identity and checked mounting. Hosts supply admitted records/services.
use tilo_desktop_model::{RoomId, Tab, TileContent};
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
    reason = "a resolution is mounted immediately; boxing the component would reshape the public variant hosts construct"
)]
pub enum TileResolution {
    Component(AppComponent),
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
            return if collab_types::first_party_app_ids::is_first_party_app_id(&id) {
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
                match TileHost::mount_component(component, move |_| wake()) {
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
pub fn tile_app_kind(tile: &TileContent) -> ui::widget::AppKind {
    use ui::widget::AppKind as Kind;
    match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => match kind {
            tilo_desktop_model::AppKind::Terminal => Kind::Terminal,
            tilo_desktop_model::AppKind::FileExplorer => Kind::Files,
            tilo_desktop_model::AppKind::Browser => Kind::Browser,
            tilo_desktop_model::AppKind::Other(id)
                if ["Notes", "Word", "Editor"]
                    .iter()
                    .any(|known| id.eq_ignore_ascii_case(known))
                    || ["so.tilo.word-processor", "so.tilo.notes"]
                        .iter()
                        .any(|known| id == known) =>
            {
                Kind::Editor
            }
            _ => Kind::Settings,
        },
        _ => Kind::Settings,
    }
}
