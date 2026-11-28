# tilo-ui-secret

The secret prompt of the Tilo UI toolkit: the sheet that asks for a password, a passcode or a
key, with its fields, notices and the passcode keypad. It sits above the toolkit (`tilo-ui`);
Tilo apps reach it as `ui::secret_prompt`.

```rust
use tilo_ui_secret::{SecretKind, SecretPrompt};
```
