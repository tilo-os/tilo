//! An Android app's secrets: sealed files in the app's no-backup directory, under a store key
//! that an AES key in the Android Keystore wraps. The Keystore key never leaves the device's
//! secure hardware (or its software keystore where there is none), so the files open only on
//! this device, in this app; a backup or a copied disk carries nothing that opens.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use jni::JNIEnv;
use jni::objects::{JByteArray, JObject, JString, JValue};
use tilo_process::android::with_activity;
use zeroize::Zeroizing;

use crate::files::{FileStore, KeyWrap};
use crate::{Protection, Secret, SecretBackend, SecretError};

/// The Keystore entry of the key that wraps the store's key.
const ALIAS: &str = "so.tilo.secrets.store-key";
/// Where the store lies under the app's no-backup directory.
const STORE_DIR: &str = "tilo-secrets";
const PROVIDER: &str = "AndroidKeyStore";
const TRANSFORMATION: &str = "AES/GCM/NoPadding";
/// What every wrap is bound to, so a wrapped value made for something else does not open.
const BOUND_TO: &[u8] = b"tilo-secrets store key";
const TAG_BITS: i32 = 128;
/// The nonce the Keystore draws for each GCM encryption.
const IV_LEN: usize = 12;
const ENCRYPT_MODE: i32 = 1;
const DECRYPT_MODE: i32 = 2;
/// `KeyProperties.PURPOSE_ENCRYPT | PURPOSE_DECRYPT`.
const PURPOSES: i32 = 1 | 2;

/// Opened on first use. A failed open is not kept: the next call tries again.
#[derive(Default)]
pub(crate) struct KeystoreStore {
    opened: Mutex<Option<Arc<FileStore>>>,
}

impl KeystoreStore {
    fn store(&self) -> Result<Arc<FileStore>, SecretError> {
        let mut opened = self
            .opened
            .lock()
            .map_err(|_| SecretError::Failed("secret store poisoned".into()))?;
        if let Some(store) = &*opened {
            return Ok(store.clone());
        }
        let store = Arc::new(FileStore::open_wrapped(&store_dir()?, &Keystore)?);
        *opened = Some(store.clone());
        drop(opened);
        Ok(store)
    }
}

impl SecretBackend for KeystoreStore {
    /// The key that opens the files is held by the Keystore, apart from them.
    fn protection(&self) -> Protection {
        Protection::OsKeychain
    }
    fn get(&self, app: &str, key: &str) -> Result<Option<Secret>, SecretError> {
        self.store()?.get(app, key)
    }
    fn put(&self, app: &str, key: &str, value: &[u8], label: &str) -> Result<(), SecretError> {
        self.store()?.put(app, key, value, label)
    }
    fn delete(&self, app: &str, key: &str) -> Result<(), SecretError> {
        self.store()?.delete(app, key)
    }
    fn probe(&self) -> Result<(), SecretError> {
        self.store().map(drop)
    }
}

fn keystore_failed(what: &str) -> impl FnOnce(String) -> SecretError + '_ {
    move |error| SecretError::Unavailable(format!("Android Keystore, {what}: {error}"))
}

/// `getNoBackupFilesDir()/tilo-secrets`: the app's own, and never in a backup.
fn store_dir() -> Result<PathBuf, SecretError> {
    with_activity(|env, activity| {
        let dir = env
            .call_method(activity, "getNoBackupFilesDir", "()Ljava/io/File;", &[])?
            .l()?;
        let path = env
            .call_method(&dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        let path: String = env.get_string(&JString::from(path))?.into();
        Ok(PathBuf::from(path).join(STORE_DIR))
    })
    .map_err(keystore_failed("the app's no-backup directory"))
}

/// The Keystore's AES key, `wrapped` as `[iv][ciphertext and tag]`.
struct Keystore;

impl KeyWrap for Keystore {
    fn wrap(&self, key: &[u8]) -> Result<Vec<u8>, SecretError> {
        let (iv, sealed) = with_activity(|env, _| {
            let secret = keystore_key(env, true)?;
            let cipher = cipher(env)?;
            env.call_method(
                &cipher,
                "init",
                "(ILjava/security/Key;)V",
                &[JValue::Int(ENCRYPT_MODE), JValue::Object(&secret)],
            )?;
            bind(env, &cipher)?;
            let iv = JByteArray::from(env.call_method(&cipher, "getIV", "()[B", &[])?.l()?);
            let iv = env.convert_byte_array(&iv)?;
            Ok((iv, do_final(env, &cipher, key)?))
        })
        .map_err(keystore_failed("wrapping the store key"))?;
        if iv.len() != IV_LEN {
            return Err(SecretError::Failed(format!(
                "the Android Keystore drew a {}-byte nonce, not {IV_LEN}",
                iv.len()
            )));
        }
        Ok([iv, sealed].concat())
    }

    fn unwrap(&self, wrapped: &[u8]) -> Result<Zeroizing<Vec<u8>>, SecretError> {
        if wrapped.len() <= IV_LEN {
            return Err(SecretError::Failed(
                "the wrapped store key is cut short".into(),
            ));
        }
        let (iv, sealed) = wrapped.split_at(IV_LEN);
        with_activity(|env, _| {
            let secret = keystore_key(env, false)?;
            if secret.is_null() {
                return Ok(None);
            }
            let cipher = cipher(env)?;
            let iv = env.byte_array_from_slice(iv)?;
            let spec = env.new_object(
                "javax/crypto/spec/GCMParameterSpec",
                "(I[B)V",
                &[JValue::Int(TAG_BITS), JValue::Object(&iv)],
            )?;
            env.call_method(
                &cipher,
                "init",
                "(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V",
                &[
                    JValue::Int(DECRYPT_MODE),
                    JValue::Object(&secret),
                    JValue::Object(&spec),
                ],
            )?;
            bind(env, &cipher)?;
            Ok(Some(Zeroizing::new(do_final(env, &cipher, sealed)?)))
        })
        .map_err(keystore_failed("unwrapping the store key"))?
        .ok_or_else(|| {
            SecretError::Unavailable(
                "the Android Keystore no longer holds the key that wraps this app's secrets".into(),
            )
        })
    }
}

/// The Keystore's key under [`ALIAS`]; a new one when there is none and `create` says so,
/// otherwise null.
fn keystore_key<'local>(
    env: &mut JNIEnv<'local>,
    create: bool,
) -> jni::errors::Result<JObject<'local>> {
    let provider = env.new_string(PROVIDER)?;
    let store = env
        .call_static_method(
            "java/security/KeyStore",
            "getInstance",
            "(Ljava/lang/String;)Ljava/security/KeyStore;",
            &[JValue::Object(&provider)],
        )?
        .l()?;
    env.call_method(
        &store,
        "load",
        "(Ljava/security/KeyStore$LoadStoreParameter;)V",
        &[JValue::Object(&JObject::null())],
    )?;
    let alias = env.new_string(ALIAS)?;
    let existing = env
        .call_method(
            &store,
            "getKey",
            "(Ljava/lang/String;[C)Ljava/security/Key;",
            &[JValue::Object(&alias), JValue::Object(&JObject::null())],
        )?
        .l()?;
    if !existing.is_null() || !create {
        return Ok(existing);
    }
    let builder_class = "android/security/keystore/KeyGenParameterSpec$Builder";
    let returns_builder = |args: &str| format!("({args})L{builder_class};");
    let builder = env.new_object(
        builder_class,
        "(Ljava/lang/String;I)V",
        &[JValue::Object(&alias), JValue::Int(PURPOSES)],
    )?;
    let modes = one_string(env, "GCM")?;
    env.call_method(
        &builder,
        "setBlockModes",
        returns_builder("[Ljava/lang/String;"),
        &[JValue::Object(&modes)],
    )?;
    let paddings = one_string(env, "NoPadding")?;
    env.call_method(
        &builder,
        "setEncryptionPaddings",
        returns_builder("[Ljava/lang/String;"),
        &[JValue::Object(&paddings)],
    )?;
    env.call_method(
        &builder,
        "setKeySize",
        returns_builder("I"),
        &[JValue::Int(256)],
    )?;
    let spec = env
        .call_method(
            &builder,
            "build",
            "()Landroid/security/keystore/KeyGenParameterSpec;",
            &[],
        )?
        .l()?;
    let algorithm = env.new_string("AES")?;
    let generator = env
        .call_static_method(
            "javax/crypto/KeyGenerator",
            "getInstance",
            "(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;",
            &[JValue::Object(&algorithm), JValue::Object(&provider)],
        )?
        .l()?;
    env.call_method(
        &generator,
        "init",
        "(Ljava/security/spec/AlgorithmParameterSpec;)V",
        &[JValue::Object(&spec)],
    )?;
    env.call_method(&generator, "generateKey", "()Ljavax/crypto/SecretKey;", &[])?
        .l()
}

fn one_string<'local>(
    env: &mut JNIEnv<'local>,
    value: &str,
) -> jni::errors::Result<JObject<'local>> {
    let value = env.new_string(value)?;
    Ok(env.new_object_array(1, "java/lang/String", &value)?.into())
}

fn cipher<'local>(env: &mut JNIEnv<'local>) -> jni::errors::Result<JObject<'local>> {
    let transformation = env.new_string(TRANSFORMATION)?;
    env.call_static_method(
        "javax/crypto/Cipher",
        "getInstance",
        "(Ljava/lang/String;)Ljavax/crypto/Cipher;",
        &[JValue::Object(&transformation)],
    )?
    .l()
}

fn bind(env: &mut JNIEnv<'_>, cipher: &JObject<'_>) -> jni::errors::Result<()> {
    let bound = env.byte_array_from_slice(BOUND_TO)?;
    env.call_method(cipher, "updateAAD", "([B)V", &[JValue::Object(&bound)])?;
    Ok(())
}

fn do_final(
    env: &mut JNIEnv<'_>,
    cipher: &JObject<'_>,
    input: &[u8],
) -> jni::errors::Result<Vec<u8>> {
    let input = env.byte_array_from_slice(input)?;
    let output = env
        .call_method(cipher, "doFinal", "([B)[B", &[JValue::Object(&input)])?
        .l()?;
    env.convert_byte_array(JByteArray::from(output))
}
