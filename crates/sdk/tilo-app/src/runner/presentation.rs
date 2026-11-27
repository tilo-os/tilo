//! Native presentation actions and their caches, selected by the SDK runner boundary.

use super::RENDER_TYPE;
use crate::framework::ViewCx;
use collab_types::app_actions::{ACTIVATE_OPERATION, AppOutline};
use std::cell::Cell;
use tilo_resource::Resource;
use tilo_ui_base::presence::PresenceCtx;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{Theme, text_render::TextRenderer};
use ui_core::automerge::AutoCommit;
use ui_core::vello::{
    Scene,
    kurbo::{Affine, Rect},
    peniko::Color,
};

pub(crate) type DrawPast = fn(AutoCommit, Rect, &mut Scene, Affine, &Theme) -> Result<(), String>;

pub(crate) struct Presentation {
    store: Option<tilo_content::ContentStore>,
    drawn_theme: Option<Theme>,
    render_text: Option<TextRenderer>,
    past: Option<DrawPast>,
    activation_requested: Cell<bool>,
    /// Whether the window's picture is cleared to the window's ground: off
    /// for a window that stands on nothing, whose picture is cleared to
    /// nothing so what is under it shows.
    ground: bool,
}

impl Presentation {
    pub(crate) fn new<R: Resource>(cx: &ViewCx<R>) -> Self {
        Self {
            store: cx.content(),
            drawn_theme: None,
            render_text: None,
            past: None,
            activation_requested: Cell::new(false),
            ground: true,
        }
    }

    /// Clear the window's picture to nothing: what the view does not paint is
    /// the window under it.
    pub(crate) fn without_ground(&mut self) {
        self.ground = false;
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

    /// Ask the window to come forward: what showing an item on the person's behalf does, so the
    /// window that shows it is the one they see.
    pub(crate) fn request_activation(&self) {
        self.activation_requested.set(true);
    }

    pub(crate) fn start_operation(
        &mut self,
        op: &tilo_ui_base::host_operation::ResourceOperation<'_>,
        doc: &AutoCommit,
        content: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<Option<tilo_actions::ActionOutput>, String> {
        let request = op.request();
        match request.type_id {
            ACTIVATE_OPERATION => {
                self.activation_requested.set(true);
                Ok(Some(tilo_actions::remote::activation_requested()))
            }
            RENDER_TYPE => {
                let args: serde_json::Value =
                    serde_json::from_slice(request.payload).map_err(|error| error.to_string())?;
                let scale = args
                    .get("scale")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(1.0);
                let version = args.get("version").and_then(serde_json::Value::as_str);
                self.render(scale, version, doc, content).map(Some)
            }
            super::OUTLINE_TYPE => {
                use tilo_resource::Effect as _;

                let AppOutline::Outline { picture, scale, .. } =
                    AppOutline::from_payload(request.payload).map_err(|error| error.to_string())?;
                let theme = self.drawn_theme.clone().unwrap_or_else(Theme::dark);
                let output = super::outline::answer(&*content, doc, &theme);
                self.outline_picture(picture.unwrap_or(false), scale, doc, content, output)
                    .map(Some)
            }
            _ => Ok(None),
        }
    }

    /// The outline `output` as the operation answers it, with the view as a picture when
    /// `picture` was asked for (as `render` draws it), at `scale` pixels per point.
    fn outline_picture(
        &mut self,
        picture: bool,
        scale: Option<f64>,
        doc: &AutoCommit,
        content: &mut dyn tilo_ui_base::widget::Widget,
        mut output: tilo_actions::ActionOutput,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if !picture {
            return Ok(output);
        }
        let drawn = self.render(scale.unwrap_or(1.0), None, doc, content)?;
        output.resources = drawn.resources;
        if let (Some(fields), Some(serde_json::Value::Object(made))) = (
            output
                .value
                .as_mut()
                .and_then(serde_json::Value::as_object_mut),
            drawn.value,
        ) {
            fields.extend(made);
        }
        Ok(output)
    }

    /// The view at `scale` pixels per point, of the document's `version` where one is named.
    fn render(
        &mut self,
        scale: f64,
        version: Option<&str>,
        doc: &AutoCommit,
        content: &mut dyn tilo_ui_base::widget::Widget,
    ) -> Result<tilo_actions::ActionOutput, String> {
        if !(0.5..=2.0).contains(&scale) {
            return Err("scale must be within 0.5..=2".into());
        }
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
        let ground = if self.ground {
            theme.tokens.colors.surfaces.content
        } else {
            Color::TRANSPARENT
        };
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
                ground,
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
                ground,
                None,
                &rect,
            );
            content.draw_child(
                &mut scene,
                transform,
                text,
                &theme,
                &PresenceCtx::empty().draw_env(),
            );
            super::outline::revision(doc)
        };
        // On the GPU, only a frame that kept all of its geometry; without an
        // adapter (a confined app worker, a machine with no GPU), on the CPU.
        let rgba = tilo_ui_base::offscreen::render_picture(&scene, width, height, ground)?;
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
    &[RENDER_TYPE, super::OUTLINE_TYPE, ACTIVATE_OPERATION]
}

pub(crate) fn with_native_presentations(
    mut resource: tilo_actions::ResourceSpec,
) -> tilo_actions::ResourceSpec {
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
    collab_types::app_actions::activate_spec()
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
            tilo_actions::ActionParam::optional(
                ANCHOR_PARAM,
                tilo_actions::ParamType::Text,
                "An item of the app's to show first: the anchor of a search hit",
            )
            .nullable(),
        ],
        |args| {
            tilo_actions::EncodedAction::encode_operation(
                RENDER_TYPE,
                &serde_json::json!({
                    "scale": args.number("scale"),
                    "version": args.text("version"),
                    ANCHOR_PARAM: args.text(ANCHOR_PARAM),
                }),
            )
            .unwrap_or_else(|_| tilo_actions::EncodedAction {
                type_id: RENDER_TYPE.into(),
                payload: Vec::new(),
            })
        },
    )
    .of_kind(tilo_actions::ActionKind::Read)
    .with_description(
        "See the app's window as a picture: answers with the image and the revision of the document it shows. With an anchor, shows that item first.",
    )
    // How an agent sees what the person sees: never a row of the person's.
    .for_audience(tilo_actions::Audience::Agents);
    spec.type_mut().contract.result = tilo_actions::ResultSpec::Json {
        schema: serde_json::json!({"type": "object"}),
    };
    spec
}

/// The `anchor` parameter, which the shared outline names for both actions.
use collab_types::app_actions::OUTLINE_ANCHOR as ANCHOR_PARAM;
