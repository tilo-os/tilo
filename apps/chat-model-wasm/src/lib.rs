//! The Chat model as the WebAssembly module the app's package carries.
//!
//! Every holder of a workspace with Chat installed applies these verbs from the
//! package, whether or not it ever builds the app: the same bytes on a laptop,
//! a phone, the page and a server.
//!
//! The module serves the workspace's chat resource: its channels and
//! categories, each person's read marks, reviewed feedback, moderation and what
//! an agent keeps of a conversation. Each person's drafts are their own, written
//! on their own devices, so no other holder applies them.

tilo_model_guest::model!("chat", chat_model::Chat, chat_model::ChatDrafts);
