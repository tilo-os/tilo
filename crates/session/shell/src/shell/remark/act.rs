//! Every step of the remark surface as a shell action.
//!
//! The pointer, the keyboard, `Ctrl+K`, an agent and `tilo-control` all
//! reach the composer through these, so there is one path through it and a
//! test that drives the actions drives what a person does. Each takes the
//! chrome state and its bound arguments, changes the composer or the pick
//! state, and queues whatever needs the broker, the gateway or the record
//! store for the host's next frame.

use vello::kurbo::{Point, Rect};

use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::{ActionArgs, ActionError, ActionParam, ParamType};
use crate::shell::session_services::ServiceIntent;

use super::pick::{NodeKey, Walk};
use super::review::{ModelUse, SetUpChoice};
use super::{Flow, Opening, Pick, Request, Stage, app_reference};

fn refused(message: impl Into<String>) -> ActionError {
    ActionError::Refused(message.into())
}

fn text<'a>(args: &'a ActionArgs, name: &str) -> Result<&'a str, ActionError> {
    args.text(name)
        .ok_or_else(|| refused(format!("`{name}` is required")))
}

fn number(args: &ActionArgs, name: &str) -> Result<f64, ActionError> {
    args.number(name)
        .ok_or_else(|| refused(format!("`{name}` is required")))
}

fn index(args: &ActionArgs, name: &str) -> Result<u32, ActionError> {
    args.integer(name)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| refused(format!("`{name}` must be a step or item number from 0")))
}

fn require_open(st: &ChromeState) -> Result<(), ActionError> {
    if st.remark.is_open() {
        Ok(())
    } else {
        Err(refused(
            "nothing is being written; say something about this first (remark.open)",
        ))
    }
}

fn require_picking(st: &ChromeState) -> Result<(), ActionError> {
    if st.remark.stage == Stage::Picking {
        Ok(())
    } else {
        Err(refused(
            "pick mode is not on; start it with remark.pick.start",
        ))
    }
}

fn require_review(st: &ChromeState) -> Result<(), ActionError> {
    if st.remark.stage == Stage::Review && st.remark.review.is_some() {
        Ok(())
    } else {
        Err(refused(
            "nothing is under review; attach a recording and open it with remark.review.open",
        ))
    }
}

/// The app with focus, as what a remark opened with nothing picked is about.
fn focused_app_context(st: &ChromeState) -> Option<Pick> {
    let group = crate::shell::window_actions::focused_group(st)?;
    let (room, tab_id, app_id, label) = {
        let tree = st.tree();
        let workspace = tree.desktop.active_workspace()?;
        let group = workspace
            .strips
            .iter()
            .flat_map(|strip| strip.window_groups.iter())
            .find(|candidate| candidate.id == group)?;
        let tab = group.tabs.iter().find(|tab| tab.id == group.active_tab)?;
        let (app_id, label) = crate::shell::remark::tab_app(tab)?;
        let found = (workspace.room.to_string(), tab.id, app_id, label);
        drop(tree);
        found
    };
    Some(Pick {
        surface: format!("tab:{tab_id}"),
        node: String::new(),
        reference: app_reference(&app_id, &label),
        app_id,
        app_label: label.clone(),
        room,
        role: "application".into(),
        label,
        personal: false,
        screen: Rect::ZERO,
    })
}

/// Open the remark surface, in pick mode, on the focused app.
///
/// # Errors
///
/// Never; opening while open returns to picking.
pub fn open(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    let context = focused_app_context(st);
    let known = st
        .session_view()
        .recorder
        .as_ref()
        .map(|recorder| recorder.awaiting_review.clone())
        .unwrap_or_default();
    let was_open = st.remark.is_open();
    let default = st.remark_directory().default_destination();
    st.remark.open(Opening::Gesture, context, default);
    if !was_open {
        st.remark.known_sessions = known;
        st.pick.clear();
    }
    st.push_remark_request(Request::RefreshPick);
    st.bump();
    Ok(())
}

/// Open the composer about an app that stopped unexpectedly.
///
/// # Errors
///
/// When `app` is missing.
pub fn open_crash(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let app_id = text(args, "app")?.to_string();
    let label = args
        .text("label")
        .map_or_else(|| app_id.clone(), ToString::to_string);
    let room = st
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room.to_string())
        .unwrap_or_default();
    let context = Pick {
        surface: String::new(),
        node: String::new(),
        app_id: app_id.clone(),
        app_label: label.clone(),
        room,
        role: "application".into(),
        label: label.clone(),
        reference: app_reference(&app_id, &label),
        personal: false,
        screen: Rect::ZERO,
    };
    st.remark.close();
    let default = st.remark_directory().default_destination();
    st.remark.open(
        Opening::Crash {
            app_id,
            app_label: label,
        },
        Some(context),
        default,
    );
    // The recent buffer, when on, already holds what led up to the crash.
    if st
        .session_view()
        .recorder
        .as_ref()
        .is_some_and(|recorder| recorder.recent_buffer)
    {
        use_recent(st, &ActionArgs::none())?;
    }
    st.bump();
    Ok(())
}

/// Close the composer and forget what was written.
///
/// # Errors
///
/// Never.
pub fn discard(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    if let Flow::Attached { record, .. } = &st.remark.flow
        && let Some(record) = ui::record::RecordId::parse(record)
    {
        // Discarded means forgotten: the session never left memory.
        st.push_service_intent(ServiceIntent::ForgetSession { record });
    }
    if matches!(st.remark.flow, Flow::Recording { .. }) {
        let user = st.active_local_user();
        st.push_service_intent(ServiceIntent::StopAndForget { user });
    }
    st.remark.close();
    st.pick.clear();
    st.bump();
    Ok(())
}

/// Back into pick mode, to add to what the remark is about.
///
/// # Errors
///
/// When nothing is being written.
pub fn pick_start(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    st.remark.stage = Stage::Picking;
    st.pick.selection.clear();
    st.push_remark_request(Request::RefreshPick);
    st.bump();
    Ok(())
}

/// Pick the node at a screen point, as a click does.
///
/// # Errors
///
/// When not picking, or nothing pickable is there.
pub fn pick_at(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    let point = Point::new(number(args, "x")?, number(args, "y")?);
    let additive = args.boolean("add").unwrap_or(false);
    st.pick.drag = None;
    if !st.pick.release(point, additive) {
        return Err(refused(format!(
            "no app on screen describes anything at ({}, {})",
            point.x, point.y
        )));
    }
    st.pick.focus = st.pick.scene.node_at(point).map(|resolved| resolved.key());
    st.bump();
    Ok(())
}

/// Pick a node by its app surface and semantic id.
///
/// # Errors
///
/// When not picking, or the surface does not describe that node.
pub fn pick_node(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    let key = NodeKey::new(text(args, "surface")?, text(args, "node")?);
    if st.pick.scene.resolve(&key).is_none() {
        return Err(refused(format!(
            "surface `{}` describes no node `{}`",
            key.surface, key.node
        )));
    }
    if args.boolean("add").unwrap_or(false) {
        st.pick.selection.add(key.clone());
    } else {
        st.pick.selection.replace(key.clone());
    }
    st.pick.focus = Some(key);
    st.bump();
    Ok(())
}

/// Pick every node a screen region sweeps, as a drag does.
///
/// # Errors
///
/// When not picking, or the region sweeps nothing.
pub fn pick_region(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    let start = Point::new(number(args, "x0")?, number(args, "y0")?);
    let end = Point::new(number(args, "x1")?, number(args, "y1")?);
    st.pick.drag = Some((start, end));
    if !st.pick.release(end, args.boolean("add").unwrap_or(false)) {
        return Err(refused("the region sweeps no node any app describes"));
    }
    st.bump();
    Ok(())
}

/// Walk the pick focus through the tree, as the arrow keys do.
///
/// # Errors
///
/// When not picking, the direction is unknown, or the walk is at an edge.
pub fn pick_walk(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    let direction = text(args, "direction")?;
    let walk = Walk::parse(direction).ok_or_else(|| {
        refused(format!(
            "`{direction}` is not a direction: use out, in, previous or next"
        ))
    })?;
    if !st.pick.walk(walk) {
        return Err(refused("nothing lies that way"));
    }
    st.bump();
    Ok(())
}

/// Pick the focused node, as Enter does on a node.
///
/// # Errors
///
/// When not picking or nothing has focus.
pub fn pick_take(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    if !st.pick.take_focus(args.boolean("toggle").unwrap_or(false)) {
        return Err(refused("nothing has pick focus; walk to a node first"));
    }
    st.bump();
    Ok(())
}

/// Leave pick mode with the selection as the remark's reference.
///
/// # Errors
///
/// When not picking.
pub fn pick_done(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    let scene = st.pick.scene.clone();
    let selection = st.pick.selection.clone();
    if selection.is_empty() {
        st.remark.cancel_picking();
    } else {
        st.remark.finish_picking(&scene, &selection);
    }
    st.pick.clear();
    st.bump();
    Ok(())
}

/// Leave pick mode without adding anything.
///
/// # Errors
///
/// When not picking.
pub fn pick_cancel(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_picking(st)?;
    st.remark.cancel_picking();
    st.pick.clear();
    st.bump();
    Ok(())
}

/// Take one picked thing back out.
///
/// # Errors
///
/// When there is no such pick.
pub fn pick_remove(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    let at = index(args, "index")?;
    if !st.remark.remove_pick(at as usize) {
        return Err(refused(format!("there is no pick {at}")));
    }
    st.bump();
    Ok(())
}

/// Set what the remark says.
///
/// # Errors
///
/// When nothing is being written.
pub fn set_text(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    st.remark.text = text(args, "text")?.to_string();
    st.bump();
    Ok(())
}

/// Choose where the remark goes.
///
/// # Errors
///
/// When the destination is not one the selector offers.
pub fn set_destination(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    let id = text(args, "id")?;
    let options = st.remark_directory().options();
    if !options.iter().any(|option| option.id == id) {
        let offered: Vec<&str> = options.iter().map(|option| option.id.as_str()).collect();
        return Err(refused(format!(
            "`{id}` is not offered here; choose one of: {}",
            offered.join(", ")
        )));
    }
    st.remark.destination = Some(id.to_string());
    st.bump();
    Ok(())
}

/// Keep the recorded windows' frames with the recording.
///
/// # Errors
///
/// When nothing is being written or `enabled` is missing.
pub fn set_frames(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    st.remark.include_frames = args
        .boolean("enabled")
        .ok_or_else(|| refused("say whether frames are included"))?;
    st.bump();
    Ok(())
}

/// The rooms a recording for this remark follows: those of the picked
/// surfaces, else the active workspace.
fn recording_rooms(st: &ChromeState) -> Vec<String> {
    let mut rooms: Vec<String> = st
        .remark
        .picks
        .iter()
        .chain(st.remark.context.iter())
        .map(|pick| pick.room.clone())
        .filter(|room| !room.is_empty())
        .collect();
    rooms.sort();
    rooms.dedup();
    if rooms.is_empty() {
        rooms.extend(
            st.tree()
                .desktop
                .active_workspace()
                .map(|workspace| workspace.room.to_string()),
        );
    }
    rooms
}

fn require_recorder(st: &ChromeState) -> Result<(), ActionError> {
    if st.session_view().recorder.is_some() {
        Ok(())
    } else {
        Err(refused(crate::shell::session_services::NO_RECORDER))
    }
}

/// "Record from now": the Recorder agent joins the rooms of what was
/// picked and records until stopped. The recorder starts on the host's
/// next frame; the remark's flow follows once it has, and a refusal shows
/// as the remark's notice.
///
/// # Errors
///
/// When a recording is attached or running, or there is no recorder.
pub fn record(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    if !matches!(st.remark.flow, Flow::None) {
        return Err(refused("this remark already has a recording"));
    }
    require_recorder(st)?;
    let rooms = recording_rooms(st);
    let user = st.active_local_user();
    // Frames are always captured for the on-device replay check; "Include
    // screen frames" is the person choosing to send them too.
    let pixels = if st.remark.include_frames {
        session_trace::frames::PixelPurpose::Keep
    } else {
        session_trace::frames::PixelPurpose::Check
    };
    st.push_service_intent(ServiceIntent::RecordForRemark {
        rooms,
        pixels,
        user,
    });
    st.bump();
    Ok(())
}

/// Stop the recording and attach it, on the host's next frame. A refusal,
/// such as nothing recording, shows as the remark's notice.
///
/// # Errors
///
/// When the composer is closed or there is no recorder.
pub fn stop(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    require_recorder(st)?;
    let user = st.active_local_user();
    st.push_service_intent(ServiceIntent::StopForRemark { user });
    st.bump();
    Ok(())
}

/// "Use the last 2 minutes": seal what the recent buffer kept, on the
/// host's next frame. A refusal, such as the buffer being off or empty,
/// shows as the remark's notice.
///
/// # Errors
///
/// When a recording is attached or running, or there is no recorder.
pub fn use_recent(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    if !matches!(st.remark.flow, Flow::None) {
        return Err(refused("this remark already has a recording"));
    }
    require_recorder(st)?;
    st.push_service_intent(ServiceIntent::CaptureRecentForRemark);
    st.bump();
    Ok(())
}

/// Take the recording off the remark, forgetting it.
///
/// # Errors
///
/// When there is no attached recording.
pub fn remove_recording(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    let Flow::Attached { record, .. } = std::mem::take(&mut st.remark.flow) else {
        return Err(refused("no recording is attached"));
    };
    if let Some(record) = ui::record::RecordId::parse(&record) {
        st.push_service_intent(ServiceIntent::ForgetSession { record });
    }
    st.remark.review = None;
    if st.remark.stage == Stage::Review {
        st.remark.stage = Stage::Compose;
    }
    st.bump();
    Ok(())
}

/// Walk through the attached recording before it is sent.
///
/// # Errors
///
/// When no recording is attached.
pub fn review_open(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    if !matches!(st.remark.flow, Flow::Attached { .. }) {
        return Err(refused("no recording is attached to review"));
    }
    st.remark.stage = Stage::Review;
    if st.remark.review.is_none() {
        st.push_remark_request(Request::OpenReview);
    }
    st.bump();
    Ok(())
}

/// Back from review to the composer, keeping every decision.
///
/// # Errors
///
/// When nothing is under review.
pub fn review_close(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    if st.remark.stage != Stage::Review {
        return Err(refused("nothing is under review"));
    }
    st.remark.stage = Stage::Compose;
    st.bump();
    Ok(())
}

fn review_mut(st: &mut ChromeState) -> Result<&mut super::review::Review, ActionError> {
    st.remark
        .review
        .as_mut()
        .ok_or_else(|| refused("the walk-through is still being built"))
}

/// Show one step of the walk-through.
///
/// # Errors
///
/// When there is no such step.
pub fn review_select(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let review = review_mut(st)?;
    if step as usize >= review.steps.len() {
        return Err(refused(format!("there is no step {step}")));
    }
    review.selected = step;
    st.bump();
    Ok(())
}

fn span(args: &ActionArgs) -> Result<tilo_feedback_review::Span, ActionError> {
    Ok(tilo_feedback_review::Span {
        start: index(args, "start")?,
        end: index(args, "end")?,
    })
}

/// Hide a run of one step's text, by byte range.
///
/// # Errors
///
/// When the step, text or range does not exist.
pub fn review_redact(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let node = text(args, "node")?.to_string();
    let span = span(args)?;
    review_mut(st)?
        .redact(step, &node, span)
        .map_err(|key| refused(crate::shell::i18n::t(key)))?;
    st.bump();
    Ok(())
}

/// Hide one text of a step whole, as pressing it in the preview does.
///
/// # Errors
///
/// When the step or text does not exist.
pub fn review_hide(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let node = text(args, "node")?.to_string();
    review_mut(st)?
        .hide(step, &node)
        .map_err(|key| refused(crate::shell::i18n::t(key)))?;
    st.bump();
    Ok(())
}

/// Hide the word at a byte offset, as a click on text does.
///
/// # Errors
///
/// When the step or text does not exist, or no word is there.
pub fn review_redact_word(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let node = text(args, "node")?.to_string();
    let at = index(args, "at")? as usize;
    review_mut(st)?
        .redact_word_at(step, &node, at)
        .map_err(|key| refused(crate::shell::i18n::t(key)))?;
    st.bump();
    Ok(())
}

/// Show again a run the person hid. What review itself hid stays hidden.
///
/// # Errors
///
/// When the person hid nothing there.
pub fn review_unredact(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let node = text(args, "node")?.to_string();
    let span = span(args)?;
    if !review_mut(st)?.unredact(step, &node, span) {
        return Err(refused(
            "you hid nothing there; what review hid on its own stays hidden",
        ));
    }
    st.bump();
    Ok(())
}

/// Cut a step from what is sent, or keep it again.
///
/// # Errors
///
/// When there is no such step.
pub fn review_cut(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let step = index(args, "step")?;
    let cut = args.boolean("cut").unwrap_or(true);
    if !review_mut(st)?.set_cut(step, cut) {
        return Err(refused(format!("there is no step {step}")));
    }
    st.bump();
    Ok(())
}

/// Offer to set up the review model in place: the configuration the AI hub
/// recommends for this device, with its alternatives. Nothing downloads
/// until one is confirmed with `remark.review.model.confirm`.
///
/// # Errors
///
/// When nothing is under review.
pub fn review_set_up_model(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    st.push_remark_request(Request::SetUpReviewModel);
    st.bump();
    Ok(())
}

fn offer_mut(st: &mut ChromeState) -> Result<(&mut Vec<SetUpChoice>, &mut bool), ActionError> {
    match &mut review_mut(st)?.model {
        ModelUse::Offer { choices, expanded } => Ok((choices, expanded)),
        _ => Err(refused(
            "no set-up offer is open; ask for one with remark.review.model.set-up",
        )),
    }
}

/// Show or hide the offer's alternatives.
///
/// # Errors
///
/// When no offer is open.
pub fn review_model_more(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let (_, expanded) = offer_mut(st)?;
    *expanded = !*expanded;
    st.bump();
    Ok(())
}

/// Set up the offered choice `choice` (0 is the recommendation): the
/// person's confirmation of what the offer shows.
///
/// # Errors
///
/// When no offer is open or it has no such choice.
pub fn review_model_confirm(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    let choice = index(args, "choice")?;
    let (choices, _) = offer_mut(st)?;
    let action = usize::try_from(choice)
        .ok()
        .and_then(|choice| choices.get(choice))
        .map(|choice| choice.action.clone())
        .ok_or_else(|| refused(format!("the offer has no choice {choice}")))?;
    st.push_remark_request(Request::ConfirmReviewModel { action });
    st.bump();
    Ok(())
}

/// Leave the offer without setting anything up.
///
/// # Errors
///
/// When no offer is open.
pub fn review_model_dismiss(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_review(st)?;
    offer_mut(st)?;
    review_mut(st)?.model = ModelUse::Floor;
    st.bump();
    Ok(())
}

/// Send the remark.
///
/// # Errors
///
/// With the reason Send is not ready, in the words the composer shows.
pub fn send(st: &mut ChromeState, _args: &ActionArgs) -> Result<(), ActionError> {
    require_open(st)?;
    let options = st.remark_directory().options();
    if let Some(blocker) = st.remark.send_blocker(&options) {
        return Err(refused(crate::shell::i18n::t(blocker)));
    }
    if matches!(st.remark.sending, super::Sending::InFlight { .. }) {
        return Err(refused(crate::shell::i18n::t("remark-sending")));
    }
    st.remark.sending = super::Sending::InFlight {
        step: "queued".into(),
    };
    st.remark.stage = Stage::Sending;
    st.push_remark_request(Request::Send);
    st.bump();
    Ok(())
}

/// Go to the workspace a remark went to, where its replies arrive in chat.
///
/// # Errors
///
/// When that room is not a workspace on this machine.
pub fn open_conversation(st: &mut ChromeState, args: &ActionArgs) -> Result<(), ActionError> {
    let room = text(args, "room")?.to_string();
    let workspace = st
        .tree()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == room)
        .map(|workspace| workspace.id)
        .ok_or_else(|| refused(format!("room {room} is not a workspace here")))?;
    st.tree_mut().set_active(workspace);
    st.bump();
    Ok(())
}

/// Parameters, for the catalogue.
pub const ROOM_PARAMS: &[ActionParam] = &[ActionParam::required(
    "room",
    ParamType::Text,
    "The room id the remark was delivered to (chrome.remark.last.room).",
)];
pub const PICK_AT_PARAMS: &[ActionParam] = &[
    ActionParam::required("x", ParamType::Number, "Screen x, in logical pixels."),
    ActionParam::required("y", ParamType::Number, "Screen y, in logical pixels."),
    ActionParam::optional(
        "add",
        ParamType::Boolean,
        "Add to the selection (Shift-click) instead of replacing it.",
    ),
];
pub const PICK_NODE_PARAMS: &[ActionParam] = &[
    ActionParam::required(
        "surface",
        ParamType::Text,
        "The surface key state-dump lists under chrome.remark.pick.surfaces.",
    ),
    ActionParam::required("node", ParamType::Text, "The node's stable semantic id."),
    ActionParam::optional("add", ParamType::Boolean, "Add instead of replacing."),
];
pub const PICK_REGION_PARAMS: &[ActionParam] = &[
    ActionParam::required("x0", ParamType::Number, "One corner's screen x."),
    ActionParam::required("y0", ParamType::Number, "One corner's screen y."),
    ActionParam::required("x1", ParamType::Number, "The other corner's screen x."),
    ActionParam::required("y1", ParamType::Number, "The other corner's screen y."),
    ActionParam::optional("add", ParamType::Boolean, "Add instead of replacing."),
];
pub const PICK_WALK_PARAMS: &[ActionParam] = &[ActionParam::required(
    "direction",
    ParamType::Text,
    "out (enclosing node), in (first child), previous or next (sibling).",
)];
pub const PICK_TAKE_PARAMS: &[ActionParam] = &[ActionParam::optional(
    "toggle",
    ParamType::Boolean,
    "Take the node back out when it is already picked.",
)];
pub const INDEX_PARAMS: &[ActionParam] = &[ActionParam::required(
    "index",
    ParamType::Integer,
    "Which pick, from 0, in the order the composer lists them.",
)];
pub const TEXT_PARAMS: &[ActionParam] = &[ActionParam::required(
    "text",
    ParamType::Text,
    "What the remark says. `@name` mentions someone in a room destination.",
)];
pub const DESTINATION_PARAMS: &[ActionParam] = &[ActionParam::required(
    "id",
    ParamType::Text,
    "team, community, room:<room id> or person:<person id>, as chrome.remark.destinations lists them.",
)];
pub const ENABLED_PARAMS: &[ActionParam] = &[ActionParam::required(
    "enabled",
    ParamType::Boolean,
    "Keep screen frames of the recorded windows, with personal fields masked.",
)];
pub const CRASH_PARAMS: &[ActionParam] = &[
    ActionParam::required("app", ParamType::Text, "The app id that stopped."),
    ActionParam::optional("label", ParamType::Text, "The app's display name."),
];
pub const STEP_PARAMS: &[ActionParam] = &[ActionParam::required(
    "step",
    ParamType::Integer,
    "The walk-through step, from 0.",
)];
pub const REDACT_PARAMS: &[ActionParam] = &[
    ActionParam::required("step", ParamType::Integer, "The walk-through step, from 0."),
    ActionParam::required("node", ParamType::Text, "The text's node id in that step."),
    ActionParam::required("start", ParamType::Integer, "First byte to hide."),
    ActionParam::required(
        "end",
        ParamType::Integer,
        "Byte after the last one to hide.",
    ),
];
pub const HIDE_PARAMS: &[ActionParam] = &[
    ActionParam::required("step", ParamType::Integer, "The walk-through step, from 0."),
    ActionParam::required("node", ParamType::Text, "The text's node id in that step."),
];
pub const REDACT_WORD_PARAMS: &[ActionParam] = &[
    ActionParam::required("step", ParamType::Integer, "The walk-through step, from 0."),
    ActionParam::required("node", ParamType::Text, "The text's node id in that step."),
    ActionParam::required("at", ParamType::Integer, "A byte offset inside the word."),
];
pub const CHOICE_PARAMS: &[ActionParam] = &[ActionParam::required(
    "choice",
    ParamType::Integer,
    "The offered choice, from 0: the recommendation, then each alternative.",
)];

pub const CUT_PARAMS: &[ActionParam] = &[
    ActionParam::required("step", ParamType::Integer, "The walk-through step, from 0."),
    ActionParam::optional(
        "cut",
        ParamType::Boolean,
        "true (default) leaves the step out of what is sent; false keeps it.",
    ),
];
