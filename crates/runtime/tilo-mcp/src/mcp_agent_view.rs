//! `tilo_view`: what a person sees, as an agent reads it. The app window that shows
//! `path`, as lines — every label, value and control, in reading order, each control's
//! action and where it stands — and, asked for, the same view as a picture.
//!
//! The lines are the view's own semantic tree: the one its window exports to assistive
//! technology, read from the widgets the window shows now. Nothing here is a second
//! rendering of the resource's data: a change the person makes, or an agent's own action,
//! shows in the next view because the window shows it.

use super::*;
use crate::images::take_images;

/// What `tilo_view` takes: the path whose view to read, and whether to answer a picture of
/// it as well.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ViewArgs {
    path: String,
    /// Also answer the view as a picture, as MCP image content.
    #[serde(default)]
    picture: bool,
    /// Pixels per point of that picture, 0.5 to 2 (default 1).
    scale: Option<f64>,
}

/// The app's action that reads its window as lines.
const OUTLINE: &str = "outline";

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `tilo_view {path}`: the lines of the view `path` names, or of the app window that
    /// shows it. The app is started without a window when it does not run, so looking at a
    /// workspace needs nothing opened first.
    pub(super) async fn agent_view(
        &self,
        args: ViewArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let parsed = parse_path(&args.path)?;
        if parsed.action.is_some() {
            return Err(
                "tilo_view looks at what a resource's view shows; run an action with tilo_do"
                    .into(),
            );
        }
        // The app must run to show anything: start it (no window of the person's) when it
        // does not run.
        match self.app_ref(&parsed.app).await? {
            AppRef::Running { .. } => {}
            AppRef::Installed { app_id, opens } => {
                if !opens {
                    return Err(Self::may_not_open(&app_id).into());
                }
                self.started_provider(&app_id, None).await?;
            }
        }
        let resolved = match self.resolve(&parsed).await? {
            (Ok(resolved), _) => resolved,
            (Err(note), _) => return Ok(Reply::ok(note)),
        };
        // The item resource the path names, which the app shows when asked: the view is
        // then of that resource, not of whatever the window happened to show.
        let anchor = resolved.node.as_ref().map(|node| node.id.clone());
        let mut arguments = serde_json::Map::new();
        if let Some(anchor) = &anchor {
            arguments.insert("anchor".into(), json!(anchor));
        }
        if args.picture {
            arguments.insert("picture".into(), json!(true));
        }
        if let Some(scale) = args.scale {
            arguments.insert("scale".into(), json!(scale));
        }
        let path = format!("{}#{OUTLINE}", parsed.app);
        let mut refused_anchor: Option<String> = None;
        let value = match Box::pin(self.do_one_value(&path, arguments.clone(), None, None)).await {
            Ok(value) => answer_value(value),
            // An app that cannot show the item (a conversation it no longer holds, an item
            // that is gone) still has a window: read that, and say which view the lines are.
            Err(why) if anchor.is_some() => {
                refused_anchor = Some(why.to_string());
                arguments.remove("anchor");
                answer_value(Box::pin(self.do_one_value(&path, arguments, None, None)).await?)
            }
            Err(why) => return Err(why),
        };
        if value.pointer("/update/state").and_then(Value::as_str) != Some("succeeded")
            || value
                .pointer("/update/output/value/outline")
                .and_then(Value::as_str)
                .is_none()
        {
            return Ok(outcome(&value));
        }
        Ok(Self::view_answer(
            &resolved,
            refused_anchor.as_deref(),
            args.picture,
            value,
            structured,
        ))
    }

    /// The answer for a view read: `value`'s lines as text, its picture as image content
    /// (or the address of it), and, for a program, the same as a typed answer.
    fn view_answer(
        resolved: &Resolved,
        refused_anchor: Option<&str>,
        wanted_picture: bool,
        mut value: Value,
        structured: bool,
    ) -> Reply {
        let output = value
            .pointer("/update/output/value")
            .cloned()
            .unwrap_or_default();
        let lines = output
            .get("outline")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // A picture the answer carries travels as image content; one this session already
        // showed is named instead, as the host's own bookkeeping decided.
        let images = pictures(&mut value);
        let mut text = String::new();
        if let Some(why) = refused_anchor {
            let _ = writeln!(
                text,
                "the app could not show {} ({why}); this is its window:",
                resolved.path
            );
        }
        text.push_str(lines.trim_end());
        // A host that could not draw the picture it was asked for says why, in its own
        // words: the lines are still the view it shows.
        if let Some(why) = output.get("picture_unavailable").and_then(Value::as_str) {
            let _ = write!(text, "\n(no picture: {why})");
        }
        // A picture this host cannot hand over as image content is named by where the
        // workspace keeps it, which its files read.
        if wanted_picture
            && images.is_empty()
            && let Some(picture) = output.get("content").and_then(Value::as_str)
        {
            let _ = write!(text, "\npicture: {picture}");
        }
        if let Some(unchanged) = value.get("unchanged").and_then(Value::as_str) {
            let _ = write!(text, "\n{unchanged}");
        }
        if text.trim().is_empty() {
            text.push_str("(the view shows nothing)");
        }
        let mut reply = Reply::ok(text);
        if structured {
            let mut answer = json!({
                "status": "ok",
                "text": lines,
                "path": resolved.path,
                "app": resolved.app_id,
                "shown": if refused_anchor.is_some() { "window" } else { "resource" },
                "nodes": output.get("nodes"),
                "omitted": output.get("omitted"),
                "viewport": output.get("viewport"),
                "window_origin": output.get("window_origin"),
                "revision": output.get("revision"),
                "tree": output.get("tree"),
            });
            if let (Some(fields), Some(picture)) = (answer.as_object_mut(), output.get("content")) {
                fields.insert("picture".into(), picture.clone());
            }
            reply.structured = Some(answer);
        }
        reply.images = images;
        reply
    }
}

/// What a finished call answered: the invocation's value.
fn answer_value(done: Done) -> Value {
    match done {
        Done::Invoked { value, .. } | Done::Launched { value, .. } => value,
    }
}

/// The image contents an answer carries, taken out of it for [`Reply::images`]: what the
/// host attached to the action's output, as the same blocks MCP image content holds.
fn pictures(value: &mut Value) -> Vec<Value> {
    take_images(value)
}
