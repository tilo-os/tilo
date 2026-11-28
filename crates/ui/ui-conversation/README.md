# tilo-ui-conversation

What a conversation is made of, once for every Tilo app that has one: its message list, its
attachments, delivery states, drafts and composer rules. It sits above the toolkit (`tilo-ui`);
Tilo apps reach it as `ui::conversation`, and tilolang apps draw `ui.ConversationList`.

```rust
use tilo_ui_conversation::{ConversationLine, ConversationList};
```
