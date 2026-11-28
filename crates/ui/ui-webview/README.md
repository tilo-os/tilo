# tilo-ui-webview

Painting a web engine's pages for the Tilo UI toolkit: the frames and layers a page streams
(`paint`), drawn into a view, and the engine and page a window hands the `ui.WebView` nodes it
builds. The engine client itself is `tilo-webview`; the `WebView` widget is `tilo-ui-model`'s.

```rust
use tilo_ui_webview::{SharedEngine, Surface, set_window_engine};
```
