//! Keyboard transfers and clicked Copy share the browser system clipboard.
//! The clipboard itself is `tilo_clipboard`'s web backend; this binds the
//! page's events to it.
use js_sys::{Function, Reflect};
use std::rc::Rc;
use ui::clipboard::events::{self, CopyReport, EventTransfer};
use wasm_bindgen::{JsCast, prelude::*};

/// Bind a write to the originating mount for this input only. No clipboard
/// payload survives the event, and late completion only reports to that mount.
pub fn during_input<R>(status: &Function, run: impl FnOnce() -> R) -> R {
    let status = status.clone();
    let report = move |report: CopyReport| {
        let json = match report {
            CopyReport::Copied => r#"{"clipboard":"copied"}"#.to_owned(),
            CopyReport::Failed(error) => serde_json::json!({
                "clipboard": "failed",
                "clipboardError": error,
            })
            .to_string(),
        };
        let _ = status.call1(&JsValue::NULL, &json.into());
    };
    events::during_input(Rc::new(report), run)
}

/// A trusted event's `DataTransfer`.
struct DataTransfer(JsValue);

impl DataTransfer {
    fn method(&self, name: &str) -> Option<Function> {
        Reflect::get(&self.0, &name.into()).ok()?.dyn_into().ok()
    }
}

impl EventTransfer for DataTransfer {
    fn set_text(&self, text: &str) -> bool {
        self.method("setData")
            .is_some_and(|f| f.call2(&self.0, &"text/plain".into(), &text.into()).is_ok())
    }

    fn text(&self) -> Option<String> {
        self.method("getData")?
            .call1(&self.0, &"text/plain".into())
            .ok()?
            .as_string()
    }
}

pub fn copied() -> bool {
    events::copied()
}

pub fn during<R>(transfer: JsValue, run: impl FnOnce() -> R) -> R {
    events::during(Rc::new(DataTransfer(transfer)), run)
}
