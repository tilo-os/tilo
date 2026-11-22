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
/// How long a view read waits for its answer. The view is the answer: a read that gave up
/// sooner told the agent nothing of what the window shows, only that it was still being
/// read. A window read for the first time on the workspace's server is opened and drawn
/// there before it answers, which takes about a second; the rest is for a server that is
/// still starting the app.
const VIEW_WAIT_MS: u64 = 15_000;
/// How long a view read may take altogether while what it shows is still arriving: the app
/// opened, and a window that answers while the resources it shows catch up (a folder being
/// opened) read again until it settles. An agent given a loading line would report the window
/// empty or broken.
const VIEW_SETTLE_MS: u64 = 20_000;
/// How often a view still loading is read again while it settles.
const VIEW_SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(250);

impl<H: ActionProvider + Send + Sync + 'static> McpServer<H> {
    /// `tilo_view {path}`: the lines of the view `path` names, or of the app window that
    /// shows it. The app is started without a window when it does not run, so looking at a
    /// workspace needs nothing opened first.
    pub(super) async fn agent_view(
        &self,
        args: ViewArgs,
        structured: bool,
    ) -> Result<Reply, Failure> {
        let parsed = Address::parse(&args.path)?;
        if parsed.action.is_some() {
            return Err(
                "tilo_view looks at what a resource's view shows; run an action with tilo_do"
                    .into(),
            );
        }
        // The app must run to show anything: start it (no window of the person's) when it
        // does not run.
        // A path naming a kind (`tasks.issues`) names what an app serves rather than
        // an app: that one answers wherever what it keeps is served.
        match self.tree.app_ref(&app_of(&parsed)).await {
            Ok(AppRef::Running { .. }) => {}
            Ok(AppRef::Installed { app_id, opens }) => {
                if !opens {
                    return Err(tilo_actions::tree::Unresolved::MayNotOpen {
                        name: app_id.clone(),
                    }
                    .to_string()
                    .into());
                }
                self.tree.started_provider(&app_id, None).await?;
            }
            Err(why) => {
                if self.tree.app_of_type(&app_of(&parsed)).await.is_none() {
                    return Err(why.into());
                }
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
        let path = format!("{}#{OUTLINE}", app_of(&parsed));
        // The whole read's bound, app start included: a view still loading past it is
        // answered with what it has.
        let settle_by = tokio::time::Instant::now()
            .checked_add(std::time::Duration::from_millis(VIEW_SETTLE_MS));
        let mut refused_anchor: Option<String> = None;
        let mut value =
            match Box::pin(self.do_one_value(&path, arguments.clone(), None, Some(VIEW_WAIT_MS)))
                .await
            {
                Ok(value) => answer_value(value),
                // No window of the app answers on this connection (a server shows none): nothing
                // has a view to read, and the calls that reach what the path names are said.
                Err(why) if shows_no_window(&why) => return Err(no_view(&args.path).into()),
                // An app that cannot show the item (a conversation it no longer holds, an item
                // that is gone) still has a window: read that, and say which view the lines are.
                Err(why) if anchor.is_some() => {
                    refused_anchor = Some(why.to_string());
                    arguments.remove("anchor");
                    answer_value(
                        Box::pin(self.do_one_value(
                            &path,
                            arguments.clone(),
                            None,
                            Some(VIEW_WAIT_MS),
                        ))
                        .await?,
                    )
                }
                Err(why) => return Err(why),
            };
        // An app that ran the read and refused what it was shown is in the
        // same place as one that could not start on it: its window is the view
        // it does show, and the refusal says how it read the address.
        if !shows_a_view(&value) && anchor.is_some() {
            if refused_anchor.is_none() {
                refused_anchor = Some(outcome(&value).text.trim().to_owned());
            }
            arguments.remove("anchor");
            value = answer_value(
                Box::pin(self.do_one_value(&path, arguments.clone(), None, Some(VIEW_WAIT_MS)))
                    .await?,
            );
        }
        if !shows_a_view(&value) {
            return Ok(outcome(&value));
        }
        // A window still loading what it shows answers a busy line and little else: read
        // again until it settles, bounded. Past the bound the answer is what it has, and
        // says it is still loading rather than leaving the agent to guess.
        if still_loading(&value) {
            value = self
                .settled_view(&path, &mut arguments, value, settle_by, args.picture)
                .await;
        }
        Ok(Self::view_answer(
            &resolved,
            refused_anchor.as_deref(),
            args.picture,
            value,
            structured,
        ))
    }

    /// A view still loading what it shows, read again until it settles: `value` replaced by
    /// the last view read before `settle_by`. A picture asked for is taken of the view as it
    /// stands when the reading ends, never of a loading one.
    async fn settled_view(
        &self,
        path: &str,
        arguments: &mut serde_json::Map<String, Value>,
        mut value: Value,
        settle_by: Option<tokio::time::Instant>,
        wanted_picture: bool,
    ) -> Value {
        let wanted_picture = wanted_picture && arguments.remove("picture").is_some();
        while let Some(left) = settle_by
            .and_then(|settle_by| settle_by.checked_duration_since(tokio::time::Instant::now()))
        {
            tokio::time::sleep(VIEW_SETTLE_POLL.min(left)).await;
            let wait_ms = settle_by
                .and_then(|settle_by| settle_by.checked_duration_since(tokio::time::Instant::now()))
                .map_or(0, |left| {
                    u64::try_from(left.as_millis()).unwrap_or(u64::MAX)
                });
            if wait_ms == 0 {
                break;
            }
            let Ok(again) =
                Box::pin(self.do_one_value(path, arguments.clone(), None, Some(wait_ms))).await
            else {
                break;
            };
            let again = answer_value(again);
            if !shows_a_view(&again) {
                break;
            }
            value = again;
            if !still_loading(&value) {
                break;
            }
        }
        if wanted_picture {
            arguments.insert("picture".into(), json!(true));
            if let Ok(again) =
                Box::pin(self.do_one_value(path, arguments.clone(), None, Some(VIEW_WAIT_MS))).await
            {
                let again = answer_value(again);
                if shows_a_view(&again) {
                    value = again;
                }
            }
        }
        value
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
        let loading = still_loading(&value);
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
        if loading {
            let _ = write!(
                text,
                "\n(still loading: the window has not finished showing what it holds)"
            );
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
                "loading": loading,
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

/// Whether `value` is a view read that answered: the invocation succeeded, with lines.
fn shows_a_view(value: &Value) -> bool {
    value.pointer("/update/state").and_then(Value::as_str) == Some("succeeded")
        && value
            .pointer("/update/output/value/outline")
            .and_then(Value::as_str)
            .is_some()
}

/// Whether the view `value` shows a window that has not finished what it shows: a control
/// whose state says it is busy (a folder opening, the resources it lists still arriving). An
/// answer that states no per-node state is read by its lines.
fn still_loading(value: &Value) -> bool {
    fn busy(node: &Value) -> bool {
        node.get("state")
            .and_then(Value::as_array)
            .is_some_and(|states| states.iter().any(|state| state == "busy"))
            || node
                .get("children")
                .and_then(Value::as_array)
                .is_some_and(|children| children.iter().any(busy))
    }
    let output = value.pointer("/update/output/value");
    let busy_node = output
        .and_then(|output| output.get("tree"))
        .and_then(Value::as_array)
        .is_some_and(|tree| tree.iter().any(busy));
    busy_node
        || output
            .and_then(|output| output.get("outline"))
            .and_then(Value::as_str)
            .is_some_and(|lines| lines.contains("[busy"))
}

/// Whether `why` says the app offers no action that reads its window: no window of it answers
/// on this connection.
fn shows_no_window(why: &Failure) -> bool {
    why.message.contains(" has no action")
}

/// What a caller is told where no window shows `path`: the calls that reach it all the same.
fn no_view(path: &str) -> String {
    let path = path.trim();
    let named = json!({ "path": path });
    format!(
        "no window shows {path} on this connection, so there is no view to read: tilo_read \
         {named} reads what it holds, tilo_ls {named} lists it, and tilo_link {named} is a \
         link a person opens to see it"
    )
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
