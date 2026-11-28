# tilo-ui-map

The map of the Tilo UI toolkit: children laid out at points on a surface that pans and zooms,
as a retained widget with its described component (`ui.Map`). A canvas, a diagram of devices
or a map of the world is one: the person places things, and the camera is the person's own
lens over them. It sits above the toolkit (`tilo-ui`); Tilo apps reach it as `ui::map`.

```rust
use tilo_ui_map::{Map, PinPlace};
```
