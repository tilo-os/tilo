//! What a window and the app it draws were sent: the batches a test reads them through.

use tilo_render_protocol::patch::local::End;
use tilo_render_protocol::patch::{self as ui, ToApp, ToUi, Transport as _};

/// Everything the window was sent since it was last read.
pub fn sent(window: &mut End) -> Vec<ToUi> {
    let mut out = Vec::new();
    while let Some(batch) = window.recv().expect("the window reads its batches") {
        out.extend(ui::decode_batch::<ToUi>(&batch).expect("a batch of the window's"));
    }
    out
}

/// Tell the app what the window did, and let it answer.
pub fn tell(instance: &mut tilolang_tilo::instance::Instance, far: &mut End, message: ToApp) {
    far.send(ui::encode_batch(&[message]).expect("a batch"))
        .expect("the window tells the app");
    instance.update();
}

/// What the window was told since the last call.
pub fn told(far: &mut End) -> Vec<ToUi> {
    let mut messages = Vec::new();
    while let Ok(Some(batch)) = far.recv() {
        messages.extend(ui::decode_batch::<ToUi>(&batch).expect("a batch"));
    }
    messages
}

/// The places the window's editors were sent to show (`reveal`), each as the editor reads it.
pub fn reveals(messages: &[ToUi]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            ToUi::Patch { patches, .. } => Some(patches),
            _ => None,
        })
        .flatten()
        .filter_map(|patch| match patch {
            ui::Patch::SetProp {
                name,
                value: Some(ui::Value::Opt(Some(ask))),
                ..
            } if name == "reveal" => match ask.as_ref() {
                ui::Value::Text(ask) => Some(ask.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}
