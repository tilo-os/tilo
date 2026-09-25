//! Exact application identity and checked mounting. Hosts supply admitted records/services.
use crate::{RoomId, Tab, TileContent};
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
}

pub fn canonical_app_id(tile: &TileContent) -> Option<String> {
    Some(match tile {
        TileContent::AppPlaceholder { kind, .. } | TileContent::Stub { kind, .. } => kind.app_id(),
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. } => app_id.clone(),
    })
}
/// A checked context is provided by the host; this interface grants no authority.
pub trait ComponentContextSource {
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
        let Some(id) = canonical_app_id(&request.tab.tile) else {
            return TileResolution::Unavailable {
                title: request.tab.title.clone(),
                detail: "Application identity unavailable".into(),
            };
        };
        let definition = match self.catalog.resolve(&id) {
            Ok(definition) => definition,
            Err(_) => {
                return if id.starts_with("dev.qos.") || id.starts_with("org.nirios.") {
                    TileResolution::Unavailable {
                        title: request.tab.title.clone(),
                        detail: format!(
                            "{id}: application integration or required service unavailable"
                        ),
                    }
                } else {
                    TileResolution::OpaqueForeign { app_id: id, reason: "No semantic application adapter; process-local pixels cannot be reconstructed".into() }
                };
            }
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
                            title: "Application unavailable".into(),
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
                    title: "Application unavailable".into(),
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
            crate::AppKind::Terminal => Kind::Terminal,
            crate::AppKind::FileExplorer => Kind::Files,
            crate::AppKind::Browser => Kind::Browser,
            crate::AppKind::Other(id)
                if ["Notes", "Word", "Editor"]
                    .iter()
                    .any(|known| id.eq_ignore_ascii_case(known))
                    || ["so.tilo.word-processor", "so.tilo.notes"].iter().any(|known| {
                        collab_types::first_party_app_ids::lookup_compatible_first_party_app_ids(
                            id, known,
                        )
                    }) =>
            {
                Kind::Editor
            }
            _ => Kind::Settings,
        },
        _ => Kind::Settings,
    }
}
