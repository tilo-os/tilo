//! Native presentation actions and their caches, selected by the SDK runner boundary.

use super::RENDER_TYPE;
use std::cell::Cell;
use ui_core::automerge::transaction::Transactable;
use ui_core::presence::PresenceCtx;
use ui_core::vello::{
    Scene,
    kurbo::{Affine, Rect},
};
use ui_core::{Theme, automerge::AutoCommit, text_render::TextRenderer};

pub(crate) type DrawPast = fn(AutoCommit, Rect, &mut Scene, Affine, &Theme) -> Result<(), String>;

pub(crate) struct Presentation {
    store: Option<tilo_content::ContentStore>,
    drawn_theme: Option<Theme>,
    render_text: Option<TextRenderer>,
    past: Option<DrawPast>,
    activation_requested: Cell<bool>,
}

impl Presentation {
    pub(crate) fn new(store: Option<tilo_content::ContentStore>) -> Self {
        Self {
            store,
            drawn_theme: None,
            render_text: None,
            past: None,
            activation_requested: Cell::new(false),
        }
    }

    pub(crate) fn drawing_past(&mut self, past: DrawPast) {
        self.past = Some(past);
    }

    pub(crate) fn track_theme(&mut self, theme: &Theme) {
        let ground = theme.tokens.colors.surfaces.content;
        if self.drawn_theme.as_ref().is_none_or(|drawn| {
            drawn.name != theme.name || drawn.tokens.colors.surfaces.content != ground
        }) {
            self.drawn_theme = Some(theme.clone());
        }
    }

    pub(crate) fn needs_redraw(&self) -> bool {
        self.activation_requested.get()
    }
    pub(crate) fn take_window_activation_intent(&self) -> bool {
        self.activation_requested.replace(false)
    }

    pub(crate) fn start_operation(
        &mut self,
        op: &ui_core::host_operation::ResourceOperation<'_>,
        doc: &AutoCommit,
        content: &mut dyn ui_core::widget::Widget,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        match request.type_id {
            tilo_actions::remote::APPLICATION_ACTIVATE_TYPE => {
                self.activation_requested.set(true);
                Ok(Some(tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({"activation_requested":true})),
                    resources: Vec::new(),
                }))
            }
            RENDER_TYPE => self.render(request.payload, doc, content).map(Some),
            _ => Ok(None),
        }
    }

    fn render(
        &mut self,
        payload: &[u8],
        doc: &AutoCommit,
        content: &mut dyn ui_core::widget::Widget,
    ) -> Result<tilo_actions::ActionOutput, String> {
        let args: serde_json::Value =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let scale = args
            .get("scale")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        if !(0.5..=2.0).contains(&scale) {
            return Err("scale must be within 0.5..=2".into());
        }
        let version = args.get("version").and_then(serde_json::Value::as_str);
        let rect = content.get_rect();
        let side = |points: f64| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "clamped to 1..=8192 pixels before the cast"
            )]
            let pixels = (points * scale).round().clamp(1.0, 8192.0) as u32;
            pixels
        };
        let (width, height) = (side(rect.width()), side(rect.height()));
        let theme = self.drawn_theme.clone().unwrap_or_else(Theme::dark);
        let text = self.render_text.get_or_insert_with(TextRenderer::new);
        let mut scene = Scene::new();
        let shown = if let Some(version) = version {
            let draw = self
                .past
                .ok_or_else(|| "this view cannot show another version".to_owned())?;
            let heads = ui_core::history_source::steps::resolve(doc, version)?;
            let past = doc
                .clone()
                .fork_at(&heads)
                .map_err(|error| format!("version {version} is not in this document: {error}"))?;
            let bounds = ui_core::vello::kurbo::Rect::new(0.0, 0.0, rect.width(), rect.height());
            let transform = Affine::scale(scale);
            scene.fill(
                ui_core::vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                &bounds,
            );
            draw(past, bounds, &mut scene, transform, &theme)?;
            crate::framework::history::version_name(&heads)
        } else {
            let transform = Affine::translate((-rect.x0, -rect.y0)).then_scale(scale);
            scene.fill(
                ui_core::vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                &rect,
            );
            content.draw(&mut scene, transform, text, &theme, &PresenceCtx::empty());
            revision(doc)
        };
        let mut offscreen = ui_core::offscreen::OffscreenRenderer::try_new(width, height)
            .map_err(|error| format!("no renderer for a {width}x{height} picture: {error}"))?;
        // A new renderer reserves room for a modest scene; a denser view
        // overflows its first frame, which then comes back blank. Answer
        // only a frame that kept all of its geometry.
        let rgba = offscreen.try_render_resilient(&scene, theme.tokens.colors.surfaces.content)?;
        let png =
            ui_core::png::encode_rgba8(width, height, &rgba).map_err(|error| error.to_string())?;
        let hash = self
            .store
            .clone()
            .unwrap_or_else(tilo_content::ContentStore::memory)
            .put(&png)
            .map_err(|error| format!("the picture could not be stored: {error}"))?;
        let address = tilo_actions::content_resource(&hash)
            .ok_or_else(|| "the picture has no content address".to_owned())?;
        let mut value = serde_json::json!({
            "width": width,
            "height": height,
            "theme": theme.name,
            "revision": shown,
            "content": address,
        });
        // Where the live view sits in the app's window, in window pixels: a
        // point of the picture plus this is where a person (or the computer
        // tool, aimed at this app) clicks it.
        if version.is_none()
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert(
                "window_origin".into(),
                serde_json::json!([rect.x0.round(), rect.y0.round()]),
            );
        }
        Ok(tilo_actions::ActionOutput {
            value: Some(value),
            resources: vec![address],
        })
    }
}

pub(crate) fn operation_types() -> &'static [&'static str] {
    &[RENDER_TYPE, tilo_actions::remote::APPLICATION_ACTIVATE_TYPE]
}

pub(crate) fn with_native_presentations(
    mut resource: tilo_actions::Resource,
) -> tilo_actions::Resource {
    for spec in [render_spec(), activate_spec()] {
        if !resource
            .actions
            .iter()
            .any(|existing| existing.id == spec.id)
        {
            resource = resource.with_action(spec);
        }
    }
    resource
}

fn activate_spec() -> tilo_actions::ActionSpec {
    let label = ui_core::I18nManager::lookup_plain("app-activate-window");
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        tilo_actions::remote::APPLICATION_ACTIVATE_ACTION,
        label.clone(),
        label,
        tilo_actions::OperationKind::View,
        tilo_actions::remote::APPLICATION_ACTIVATE_TYPE,
        Vec::new(),
        |_| tilo_actions::EncodedAction {
            type_id: tilo_actions::remote::APPLICATION_ACTIVATE_TYPE.into(),
            payload: b"{}".to_vec(),
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Read)
    .with_description(ui_core::I18nManager::lookup_plain(
        "app-activate-window-description",
    ));
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

fn render_spec() -> tilo_actions::ActionSpec {
    let mut spec = tilo_actions::ActionSpec::typed_operation(
        "render",
        "See the app",
        "See the app",
        tilo_actions::OperationKind::View,
        RENDER_TYPE,
        vec![
            tilo_actions::ActionParam::optional(
                "scale",
                tilo_actions::ParamType::Number,
                "Pixels per point, 0.5 to 2 (default 1)",
            )
            .nullable(),
            tilo_actions::ActionParam::optional(
                "version",
                tilo_actions::ParamType::Text,
                "A past version: change id from history, or checkpoint name",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                RENDER_TYPE,
                &serde_json::json!({ "scale": args.number("scale"), "version": args.text("version") }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: RENDER_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Read)
    .with_description(
        "See the app's window as a picture: answers with the image and the revision of the document it shows.",
    );
    spec.contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

fn revision(doc: &AutoCommit) -> String {
    let mut heads: Vec<String> = doc
        .base_heads()
        .iter()
        .map(|head| head.to_string().chars().take(12).collect())
        .collect();
    heads.sort();
    heads.join("+")
}
