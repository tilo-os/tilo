//! The conversation in the middle of the window: Chat's own view of the thread, drawn by the
//! shell on this device's copy of Chat's resource ([`ui::window_share`]). The window writes no
//! chat of its own: it says where the conversation stands and the shell draws Chat there.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::prelude::*;
use ui::tilo_workspace_agent::WorkspaceAgents;
use ui::widget::{AppModel, ModelPath};
use ui::window_share::{Embed, EmbedState, ResourceView};

use crate::sources::Assistant;
use crate::{Hands, Window, tr, trf};

/// The embed the window asks its shell for, as the place in view last said it.
pub(crate) type Wanted = Rc<RefCell<Option<Embed>>>;

/// The name the window's one embed goes by.
const KEY: &str = "assistant.conversation";

/// The agent `id` as an item of the workspace's agents: what Chat opens its conversation by.
pub(crate) fn agent_resource(id: &str) -> String {
    ui::tilo_resource::items::item_resource_id::<WorkspaceAgents>("agents", &Id::from(id))
}

/// Chat's view of the conversation with the agent `id`: Chat's resource of the workspace, shown
/// at that agent.
pub(crate) fn chat_view(id: &str) -> ResourceView {
    ResourceView {
        app: ui::apps::CHAT.to_owned(),
        record: String::new(),
        resource: agent_resource(id),
    }
}

/// What stands where the conversation goes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Shows {
    /// No shell draws another app's view here.
    Elsewhere,
    /// The shell is starting Chat's view.
    Opening,
    /// The shell cannot show it; why.
    Unavailable(String),
    /// Chat's view is drawn here.
    Live,
}

fn shows() -> Shows {
    match (ui::window_share::state(KEY), ui::window_share::drawn()) {
        (Some(EmbedState::Showing), _) => Shows::Live,
        (Some(EmbedState::Unavailable(why)), _) => Shows::Unavailable(why),
        (_, true) => Shows::Opening,
        (_, false) => Shows::Elsewhere,
    }
}

/// The conversation with `assistant`, where the window's content column stands.
pub(crate) fn view(cx: &ViewCx<Window>, hands: &Hands, assistant: &Assistant) -> Element {
    let state = cx.local(shows());
    let wanted = Rc::clone(&hands.wanted);
    let (hands, with) = (hands.clone(), assistant.clone());
    let content = cx.switch(&state, move |_cx, shows| -> Element {
        let named = |key: &str| trf(key, &[("name", with.name.clone().into())]);
        let open = |state: EmptyState| {
            let (hands, with) = (hands.clone(), with.clone());
            state
                .with_action(tr("assistant-open-in-chat"))
                .with_action_id("assistant-open-in-chat")
                .on_open(move || hands.open_in_chat(&with))
        };
        match shows {
            Shows::Elsewhere => Box::new(open(
                EmptyState::new(named("assistant-conversation-in-chat"))
                    .with_icon(Icon::MessageCircle)
                    .with_id("assistant-conversation-elsewhere"),
            )),
            Shows::Opening => Box::new(
                EmptyState::new(tr("assistant-conversation-opening"))
                    .busy()
                    .with_id("assistant-conversation-opening"),
            ),
            Shows::Unavailable(why) => Box::new(open(
                EmptyState::error(tr("assistant-conversation-unavailable"), why.clone())
                    .with_id("assistant-conversation-unavailable"),
            )),
            // Chat's view stands over this: it names the region for whoever reads the window
            // without seeing it.
            Shows::Live => Box::new(
                EmptyState::new(named("assistant-conversation-with"))
                    .with_id("assistant-conversation-live"),
            ),
        }
    });
    Box::new(Slot {
        content,
        view: chat_view(&assistant.id),
        label: trf(
            "assistant-conversation-with",
            &[("name", assistant.name.clone().into())],
        ),
        wanted,
        state,
    })
}

/// Where Chat's view goes: it asks for the embed at the room it was laid out in, and follows
/// how the shell says the view is coming along.
struct Slot {
    content: Element,
    view: ResourceView,
    /// What the conversation is called to whoever reads the window without seeing it.
    label: String,
    wanted: Wanted,
    state: LocalState<Shows>,
}

impl Slot {
    fn follow(&self) {
        let rect = self.content.get_rect();
        if rect.width() > 0.0 && rect.height() > 0.0 {
            // The person works in the conversation where it stands: the shell lays Chat out
            // at this room's own size and hands it the pointer and the keyboard there.
            *self.wanted.borrow_mut() = Some(
                Embed::miniature(KEY, self.view.clone(), rect, rect).to_work_in(self.label.clone()),
            );
        }
        let now = shows();
        if self.state.get() != now {
            self.state.set(now);
        }
    }
}

impl ui::widget::Decorator for Slot {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.follow();
        self.content.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.follow();
        self.content.update(state, path)
    }

    fn name(&self) -> &'static str {
        "AssistantConversation"
    }
}

/// The window, telling its shell which view it embeds: the one the place in view asked for
/// since the window last said, and none once that place is gone.
pub(crate) struct Embedding {
    page: Element,
    wanted: Wanted,
}

impl Embedding {
    pub(crate) fn around(page: Element, wanted: Wanted) -> Self {
        Self { page, wanted }
    }

    fn declare(&self) {
        let embed = self.wanted.borrow_mut().take();
        ui::window_share::declare(embed.into_iter().collect());
    }
}

impl ui::widget::Decorator for Embedding {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.page.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.page.as_mut()
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.page.sync(state, path);
        self.declare();
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let changed = self.page.update(state, path);
        self.declare();
        changed
    }

    fn name(&self) -> &'static str {
        "AssistantWindow"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_conversation_is_chats_view_of_the_agent() {
        let view = chat_view("assistant");
        assert_eq!(view.app, "so.tilo.chat");
        assert_eq!(view.record, "", "Chat's own document of the workspace");
        assert_eq!(
            ui::tilo_resource::items::item_of_resource::<WorkspaceAgents>(&view.resource)
                .map(|(field, id)| (field, id.as_str().to_owned())),
            Some(("agents", "assistant".to_owned())),
            "the address Chat opens an agent's conversation by"
        );
    }
}
