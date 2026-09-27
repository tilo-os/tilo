//! The Java half of the Android services: Tilo's classes in the APK
//! (`crates/sdk/ui-core/android/java/so/tilo/android`) and the calls they make
//! back into this library once the user answers a chooser or a permission
//! prompt, or the location service answers.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};

use jni::objects::{GlobalRef, JClass, JObject, JObjectArray, JString, JValue};
use jni::sys::{jboolean, jdouble, jfloat, jint, jlong};
use jni::{JNIEnv, NativeMethod};

use crate::location::LocationOutcome;
use crate::wake::Reply;

pub(super) const RESULTS: &str = "so.tilo.android.TiloActivityResults";
pub(super) const DOCUMENTS: &str = "so.tilo.android.TiloDocuments";
pub(super) const NOTIFICATIONS: &str = "so.tilo.android.TiloNotifications";
pub(super) const SPEECH: &str = "so.tilo.android.TiloSpeech";
pub(super) const LOCATION: &str = "so.tilo.android.TiloLocation";

/// A Tilo class from the APK, with its native methods registered.
///
/// Loaded through the activity's class loader: on a thread this library
/// attached, `FindClass` searches the system loader, which cannot see app
/// classes.
pub(super) fn class<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject<'_>,
    name: &'static str,
) -> jni::errors::Result<JClass<'local>> {
    static CLASSES: Mutex<Vec<(&'static str, GlobalRef)>> = Mutex::new(Vec::new());
    let cached = lock(&CLASSES)
        .iter()
        .find(|(loaded, _)| *loaded == name)
        .map(|(_, class)| class.clone());
    if let Some(class) = cached {
        return env.new_local_ref(class.as_obj()).map(JClass::from);
    }
    let loader = env
        .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?
        .l()?;
    let binary_name = env.new_string(name)?;
    let class = JClass::from(
        env.call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&binary_name)],
        )?
        .l()?,
    );
    let natives = natives(name);
    if !natives.is_empty() {
        env.register_native_methods(&class, &natives)?;
    }
    let global = env.new_global_ref(&class)?;
    lock(&CLASSES).push((name, global));
    Ok(class)
}

fn natives(class: &str) -> Vec<NativeMethod> {
    match class {
        RESULTS => vec![
            NativeMethod {
                name: "deliverResult".into(),
                sig: "(JI[Ljava/lang/String;Ljava/lang/String;)V".into(),
                fn_ptr: deliver_result as *mut c_void,
            },
            NativeMethod {
                name: "deliverPermissions".into(),
                sig: "(JZ)V".into(),
                fn_ptr: deliver_permissions as *mut c_void,
            },
        ],
        LOCATION => vec![NativeMethod {
            name: "deliver".into(),
            sig: "(JZDDFLjava/lang/String;)V".into(),
            fn_ptr: deliver_location as *mut c_void,
        }],
        _ => Vec::new(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Callbacks waiting for Java, keyed by the token Java hands back.
struct Waiters<T>(OnceLock<Mutex<HashMap<jlong, T>>>);

impl<T> Waiters<T> {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn insert(&self, waiter: T) -> jlong {
        static NEXT_TOKEN: AtomicI64 = AtomicI64::new(1);
        let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
        lock(self.0.get_or_init(Mutex::default)).insert(token, waiter);
        token
    }

    fn take(&self, token: jlong) -> Option<T> {
        lock(self.0.get_or_init(Mutex::default)).remove(&token)
    }
}

/// What `onActivityResult` delivered for one request.
pub(super) struct ActivityResult {
    pub code: i32,
    pub uris: Vec<String>,
    /// Why the request never reached a chooser.
    pub error: Option<String>,
}

type ResultWaiter = Box<dyn FnOnce(ActivityResult) + Send>;
type PermissionWaiter = Box<dyn FnOnce(Result<bool, String>) + Send>;

static RESULT_WAITERS: Waiters<ResultWaiter> = Waiters::new();
static PERMISSION_WAITERS: Waiters<PermissionWaiter> = Waiters::new();
static LOCATION_WAITERS: Waiters<Reply<LocationOutcome>> = Waiters::new();

/// Start the intent `intent` builds for a result, which reaches `on_result`
/// on the activity's UI thread. When the intent cannot be started,
/// `on_result` hears why at once instead.
pub(super) fn start_for_result(
    intent: impl for<'local> FnOnce(
        &mut JNIEnv<'local>,
        &JObject<'_>,
    ) -> jni::errors::Result<JObject<'local>>,
    on_result: impl FnOnce(ActivityResult) + Send + 'static,
) {
    let token = RESULT_WAITERS.insert(Box::new(on_result));
    let started = super::with_activity(|env, activity| {
        let results = class(env, activity, RESULTS)?;
        let intent = intent(env, activity)?;
        env.call_static_method(
            &results,
            "startForResult",
            "(Landroid/app/Activity;Landroid/content/Intent;J)V",
            &[
                JValue::Object(activity),
                JValue::Object(&intent),
                JValue::Long(token),
            ],
        )?;
        Ok(())
    });
    if let Err(error) = started
        && let Some(on_result) = RESULT_WAITERS.take(token)
    {
        on_result(ActivityResult {
            code: crate::saf::RESULT_CANCELED,
            uris: Vec::new(),
            error: Some(format!("could not show the chooser: {error}")),
        });
    }
}

/// Whether the app holds `permission`.
pub(super) fn is_granted(permission: &str) -> Result<bool, String> {
    super::with_activity(|env, activity| {
        let results = class(env, activity, RESULTS)?;
        let permission = env.new_string(permission)?;
        env.call_static_method(
            &results,
            "isGranted",
            "(Landroid/app/Activity;Ljava/lang/String;)Z",
            &[JValue::Object(activity), JValue::Object(&permission)],
        )?
        .z()
    })
}

/// Ask the user for `permissions`; `on_answer` learns whether at least one
/// was granted, or why the prompt could not be shown.
pub(super) fn request_permissions(
    permissions: &[&str],
    on_answer: impl FnOnce(Result<bool, String>) + Send + 'static,
) {
    let token = PERMISSION_WAITERS.insert(Box::new(on_answer));
    let requested = super::with_activity(|env, activity| {
        let results = class(env, activity, RESULTS)?;
        let permissions = string_array(env, permissions)?;
        env.call_static_method(
            &results,
            "requestPermissions",
            "(Landroid/app/Activity;[Ljava/lang/String;J)V",
            &[
                JValue::Object(activity),
                JValue::Object(&permissions),
                JValue::Long(token),
            ],
        )?;
        Ok(())
    });
    if let Err(error) = requested
        && let Some(on_answer) = PERMISSION_WAITERS.take(token)
    {
        on_answer(Err(format!("could not ask for permission: {error}")));
    }
}

/// Ask the location service for one fix, answered on `answer`.
pub(super) fn locate(answer: Reply<LocationOutcome>) {
    let token = LOCATION_WAITERS.insert(answer);
    let requested = super::with_activity(|env, activity| {
        let location = class(env, activity, LOCATION)?;
        env.call_static_method(
            &location,
            "locate",
            "(Landroid/content/Context;J)V",
            &[JValue::Object(activity), JValue::Long(token)],
        )?;
        Ok(())
    });
    if let Err(error) = requested
        && let Some(answer) = LOCATION_WAITERS.take(token)
    {
        answer.send(LocationOutcome::Unavailable(format!(
            "could not ask the location service: {error}"
        )));
    }
}

/// `Build.VERSION.SDK_INT`, or 0 when it cannot be read.
pub(super) fn sdk_int() -> i32 {
    static SDK_INT: OnceLock<i32> = OnceLock::new();
    *SDK_INT.get_or_init(|| {
        super::with_activity(|env, _| {
            env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?
                .i()
        })
        .unwrap_or(0)
    })
}

pub(super) fn string_array<'local>(
    env: &mut JNIEnv<'local>,
    strings: &[impl AsRef<str>],
) -> jni::errors::Result<JObjectArray<'local>> {
    let length = i32::try_from(strings.len()).unwrap_or(i32::MAX);
    let array = env.new_object_array(length, "java/lang/String", JObject::null())?;
    for (index, string) in (0..length).zip(strings) {
        let string = env.new_string(string.as_ref())?;
        env.set_object_array_element(&array, index, string)?;
    }
    Ok(array)
}

/// A Java string, or `None` for `null`.
pub(super) fn optional_string(
    env: &mut JNIEnv<'_>,
    string: JObject<'_>,
) -> jni::errors::Result<Option<String>> {
    if string.is_null() {
        return Ok(None);
    }
    Ok(Some(env.get_string(&JString::from(string))?.into()))
}

fn strings(env: &mut JNIEnv<'_>, array: &JObjectArray<'_>) -> jni::errors::Result<Vec<String>> {
    if array.is_null() {
        return Ok(Vec::new());
    }
    let length = env.get_array_length(array)?;
    let mut strings = Vec::new();
    for index in 0..length {
        let element = env.get_object_array_element(array, index)?;
        if let Some(string) = optional_string(env, element)? {
            strings.push(string);
        }
    }
    Ok(strings)
}

extern "system" fn deliver_result<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    token: jlong,
    code: jint,
    uris: JObjectArray<'local>,
    error: JString<'local>,
) {
    let Some(waiter) = RESULT_WAITERS.take(token) else {
        return;
    };
    let result = strings(&mut env, &uris).and_then(|uris| {
        let error = optional_string(&mut env, error.into())?;
        Ok(ActivityResult { code, uris, error })
    });
    waiter(result.unwrap_or_else(|error| ActivityResult {
        code,
        uris: Vec::new(),
        error: Some(format!("could not read the chooser's answer: {error}")),
    }));
}

extern "system" fn deliver_permissions<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    token: jlong,
    granted: jboolean,
) {
    if let Some(waiter) = PERMISSION_WAITERS.take(token) {
        waiter(Ok(granted != 0));
    }
}

extern "system" fn deliver_location<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    token: jlong,
    located: jboolean,
    lat: jdouble,
    lon: jdouble,
    accuracy: jfloat,
    reason: JString<'local>,
) {
    let Some(answer) = LOCATION_WAITERS.take(token) else {
        return;
    };
    let outcome = if located != 0 {
        LocationOutcome::Located {
            lat,
            lon,
            accuracy_m: f64::from(accuracy),
        }
    } else {
        let reason = optional_string(&mut env, reason.into()).ok().flatten();
        LocationOutcome::Unavailable(
            reason.unwrap_or_else(|| "the location service gave no reason".to_string()),
        )
    };
    answer.send(outcome);
}
