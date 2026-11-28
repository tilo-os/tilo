# tilo-ui-keypad

The key pad of Tilo's on-screen keyboard: rows of key caps laid out by their widths, with taps,
holds, swipes and caret drags reported as data. It sits above the toolkit (`tilo-ui`); Tilo
apps reach it as `ui::key_pad`, and tilolang apps as `ui.KeyPad`.

```rust
use tilo_ui_keypad::{KeyPad, PadKey};
```
