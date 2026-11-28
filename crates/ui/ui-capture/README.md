# tilo-ui-capture

The capture studio of the Tilo UI toolkit (`ui.CaptureStudio`): taking a screenshot, recording
the screen or a window, and marking a screenshot up before it is kept or sent. It sits above the
toolkit (`tilo-ui`); Tilo apps reach it as `ui::capture`.

```rust
use tilo_ui_capture::{CaptureMode, CaptureStudio, ScreenshotMarkup};
```
