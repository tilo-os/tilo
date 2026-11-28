//! The Rust half of a push that arrives while the app is closed: the receiver in the APK
//! (`so.tilo.android.TiloPush`) hands it the bytes, and this opens them with the device's own key
//! and answers what the notification says.
//!
//! The key is the app's own, kept in its files by [`crate::unified_push`], and it is the same key
//! the app registers with its host while it runs: a push and the app's own reading of the log name
//! one thing one way, so the notification a push shows is replaced, never doubled, when the app
//! opens.

use std::path::Path;

use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JObject, JObjectArray, JString};
use jni::sys::jobjectArray;

/// Open the push `body` and answer `[tag, title, body]`, then each button's label and link, the
/// title and body empty where what it showed waits no more; null where this device does not read
/// it. `files` is the app's `getFilesDir()`: a push may start the process with no activity, and so with none of the launch
/// environment the app's own places are read from.
#[unsafe(no_mangle)]
pub extern "system" fn Java_so_tilo_android_TiloPush_nativePush<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    files: JString<'local>,
    body: JByteArray<'local>,
) -> jobjectArray {
    let reply = open(&mut env, &files, &body);
    match reply {
        Ok(array) => JObject::from(array).into_raw(),
        Err(error) => {
            tracing::warn!(%error, "a push the distributor carried is not read");
            std::ptr::null_mut()
        }
    }
}

/// What the push says the home-screen widget shows, kept where the widget reads it: the app is
/// closed, so nothing else of the phone writes it.
fn remember(data: &Path, push: &crate::unified_push::Push) -> Result<(), String> {
    crate::telling::Snapshot::remember(&data.join(crate::telling::WIDGET_FILE), push)
}

fn open<'local>(
    env: &mut JNIEnv<'local>,
    files: &JString<'local>,
    body: &JByteArray<'local>,
) -> Result<JObjectArray<'local>, String> {
    let files: String = env
        .get_string(files)
        .map_err(|error| error.to_string())?
        .into();
    let data = Path::new(&files).join("data");
    let key = crate::unified_push::Key::load_or_create(&data)?;
    let body = env
        .convert_byte_array(body)
        .map_err(|error| error.to_string())?;
    let plain = key.open(&body)?;
    let push = crate::unified_push::Push::read(&plain)?;
    // The home-screen widget shows what a push says too; the app may not run to write it, and a
    // widget that cannot be written is no reason to lose the notice.
    if let Err(error) = remember(&data, &push) {
        tracing::warn!(%error, "a push is not kept for the widget");
    }
    let tag = crate::telling::tag_named(&push.tag);
    let mut fields = vec![tag.clone()];
    if push.gone {
        fields.extend([String::new(), String::new()]);
    } else {
        fields.extend([push.title, push.body]);
        // Each button answers as one the app shows while it runs: by the link it opens.
        let thing = hex::encode(&tag);
        for button in push
            .actions
            .iter()
            .take(tilo_process_services::notify::MAX_BUTTONS)
        {
            let link = format!(
                "{}{thing}/{}",
                crate::telling::LINK,
                hex::encode(&button.id)
            );
            fields.extend([button.label.clone(), link]);
        }
    }
    let len = i32::try_from(fields.len()).unwrap_or_default();
    let array = env
        .new_object_array(len, "java/lang/String", JObject::null())
        .map_err(|error| error.to_string())?;
    for (index, text) in fields.iter().enumerate() {
        let text = env.new_string(text).map_err(|error| error.to_string())?;
        env.set_object_array_element(&array, i32::try_from(index).unwrap_or_default(), &text)
            .map_err(|error| error.to_string())?;
    }
    Ok(array)
}
