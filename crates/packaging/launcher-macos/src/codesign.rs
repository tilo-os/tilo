//! Code-signature inspection through Security.framework.
//!
//! [`inspect`] answers, for a file on disk, the questions
//! [`crate::classify::decide`] needs: is there a signature, does it verify
//! under strict validation, is it ad-hoc, does its signer satisfy the
//! [`TrustRequirement`] (notarization included), and does it enable App
//! Sandbox. [`verify_process`] asks the identity and entitlement questions
//! again of a *running* process, which is what closes the window between
//! inspecting a file and exec'ing it.
//!
//! Every failure is a verdict, never an error to route around: an API that
//! does not answer yields [`CodeSignature::Unreadable`] or
//! [`CodeSignature::Invalid`], both of which mean Seatbelt.

use std::ffi::c_void;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::Command;

use crate::classify::{
    APP_SANDBOX_ENTITLEMENT, CodeSignature, MacLaunchMode, TrustRequirement, decide,
};

type CFTypeRef = *const c_void;
type CFIndex = isize;
type CFTypeID = usize;
type OSStatus = i32;
type SecCSFlags = u32;

#[repr(C)]
struct CFDictionaryCallBacks {
    _opaque: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> CFTypeID;
    fn CFBooleanGetTypeID() -> CFTypeID;
    fn CFBooleanGetValue(boolean: CFTypeRef) -> u8;
    fn CFDictionaryGetTypeID() -> CFTypeID;
    fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: CFIndex,
        key_callbacks: *const CFDictionaryCallBacks,
        value_callbacks: *const CFDictionaryCallBacks,
    ) -> CFTypeRef;
    fn CFNumberGetTypeID() -> CFTypeID;
    fn CFNumberGetValue(number: CFTypeRef, kind: CFIndex, value: *mut c_void) -> u8;
    fn CFNumberCreate(allocator: CFTypeRef, kind: CFIndex, value: *const c_void) -> CFTypeRef;
    fn CFStringCreateWithBytes(
        allocator: CFTypeRef,
        bytes: *const u8,
        len: CFIndex,
        encoding: u32,
        external: u8,
    ) -> CFTypeRef;
    fn CFURLCreateFromFileSystemRepresentation(
        allocator: CFTypeRef,
        buffer: *const u8,
        len: CFIndex,
        is_directory: u8,
    ) -> CFTypeRef;
}

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoFlags: CFTypeRef;
    static kSecCodeInfoEntitlementsDict: CFTypeRef;
    static kSecGuestAttributePid: CFTypeRef;
    fn SecStaticCodeCreateWithPath(
        path: CFTypeRef,
        flags: SecCSFlags,
        code: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecStaticCodeCheckValidity(
        code: CFTypeRef,
        flags: SecCSFlags,
        requirement: CFTypeRef,
    ) -> OSStatus;
    fn SecCodeCheckValidity(code: CFTypeRef, flags: SecCSFlags, requirement: CFTypeRef)
    -> OSStatus;
    fn SecRequirementCreateWithString(
        text: CFTypeRef,
        flags: SecCSFlags,
        requirement: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecCodeCopySigningInformation(
        code: CFTypeRef,
        flags: SecCSFlags,
        info: *mut CFTypeRef,
    ) -> OSStatus;
    fn SecCodeCopyGuestWithAttributes(
        host: CFTypeRef,
        attributes: CFTypeRef,
        flags: SecCSFlags,
        guest: *mut CFTypeRef,
    ) -> OSStatus;
}

const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const K_CF_NUMBER_SINT64: CFIndex = 4;
const K_CF_NUMBER_SINT32: CFIndex = 3;

// SecStaticCode.h / CSCommon.h
const K_SEC_CS_CHECK_ALL_ARCHITECTURES: SecCSFlags = 1 << 0;
const K_SEC_CS_CHECK_NESTED_CODE: SecCSFlags = 1 << 3;
const K_SEC_CS_STRICT_VALIDATE: SecCSFlags = 1 << 4;
const K_SEC_CS_RESTRICT_SYMLINKS: SecCSFlags = 1 << 7;
const K_SEC_CS_RESTRICT_SIDEBAND_DATA: SecCSFlags = 1 << 9;
const K_SEC_CS_SIGNING_INFORMATION: SecCSFlags = 1 << 1;
const K_SEC_CS_REQUIREMENT_INFORMATION: SecCSFlags = 1 << 2;
const K_SEC_CS_DYNAMIC_INFORMATION: SecCSFlags = 1 << 3;
/// `kSecCodeSignatureAdhoc` in `kSecCodeInfoFlags`.
const K_SEC_CODE_SIGNATURE_ADHOC: i64 = 0x0002;
/// `errSecCSUnsigned`.
const ERR_SEC_CS_UNSIGNED: OSStatus = -67062;

/// Strict validation: every architecture, nested code, no symlinks leaving
/// the bundle, no resources stored outside the signature.
const STRICT: SecCSFlags = K_SEC_CS_CHECK_ALL_ARCHITECTURES
    | K_SEC_CS_CHECK_NESTED_CODE
    | K_SEC_CS_STRICT_VALIDATE
    | K_SEC_CS_RESTRICT_SYMLINKS
    | K_SEC_CS_RESTRICT_SIDEBAND_DATA;

/// What `SecCodeCheckValidity` accepts of [`STRICT`] for a running process;
/// the on-disk layout checks are static-only and rejected as invalid flags.
const STRICT_DYNAMIC: SecCSFlags = K_SEC_CS_STRICT_VALIDATE;

/// An owned CoreFoundation reference, released on drop.
struct Cf(CFTypeRef);

impl Cf {
    fn new(raw: CFTypeRef) -> Option<Self> {
        (!raw.is_null()).then_some(Self(raw))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null reference this wrapper owns (a
        // Create/Copy result), released exactly once.
        unsafe { CFRelease(self.0) };
    }
}

fn cf_string(text: &str) -> Option<Cf> {
    let len = CFIndex::try_from(text.len()).ok()?;
    // SAFETY: `text` is valid UTF-8 of `len` bytes, copied by the call.
    Cf::new(unsafe {
        CFStringCreateWithBytes(
            std::ptr::null(),
            text.as_ptr(),
            len,
            K_CF_STRING_ENCODING_UTF8,
            0,
        )
    })
}

fn requirement(text: &str) -> Result<Cf, OSStatus> {
    let text = cf_string(text).ok_or(-1)?;
    let mut out: CFTypeRef = std::ptr::null();
    // SAFETY: `text` is a live CFString; `out` receives an owned reference.
    let status = unsafe { SecRequirementCreateWithString(text.0, 0, &raw mut out) };
    if status != 0 {
        return Err(status);
    }
    Cf::new(out).ok_or(-1)
}

/// Inspect the code signature of the file or bundle at `path`.
pub fn inspect(path: &Path, trust: &TrustRequirement) -> CodeSignature {
    let bytes = path.as_os_str().as_bytes();
    let Ok(len) = CFIndex::try_from(bytes.len()) else {
        return unreadable("path too long");
    };
    // SAFETY: `bytes` is valid for `len` bytes and copied by the call.
    let Some(url) = Cf::new(unsafe {
        CFURLCreateFromFileSystemRepresentation(std::ptr::null(), bytes.as_ptr(), len, 0)
    }) else {
        return unreadable("the path is not representable as a URL");
    };
    let mut raw: CFTypeRef = std::ptr::null();
    // SAFETY: `url` is a live CFURL; `raw` receives an owned SecStaticCode.
    let status = unsafe { SecStaticCodeCreateWithPath(url.0, 0, &raw mut raw) };
    if status != 0 {
        return unreadable(&format!("SecStaticCodeCreateWithPath returned {status}"));
    }
    let Some(code) = Cf::new(raw) else {
        return unreadable("SecStaticCodeCreateWithPath returned no code");
    };

    // SAFETY: `code` is a live SecStaticCode; a null requirement checks the
    // signature alone.
    match unsafe { SecStaticCodeCheckValidity(code.0, STRICT, std::ptr::null()) } {
        0 => {}
        ERR_SEC_CS_UNSIGNED => return CodeSignature::Unsigned,
        status => return CodeSignature::Invalid { status },
    }

    let info = match signing_information(
        &code,
        K_SEC_CS_SIGNING_INFORMATION | K_SEC_CS_REQUIREMENT_INFORMATION,
    ) {
        Ok(info) => info,
        Err(status) => return CodeSignature::Invalid { status },
    };
    let app_sandbox = info.app_sandbox;
    if info.ad_hoc {
        return CodeSignature::AdHoc { app_sandbox };
    }

    let trusted = match requirement(&trust.static_requirement()) {
        // SAFETY: both references are live for the call.
        Ok(req) => (unsafe { SecStaticCodeCheckValidity(code.0, STRICT, req.0) }) == 0,
        Err(status) => {
            tracing::error!(
                target: "tilo_launcher::macos",
                status,
                "the trust requirement does not compile; no signer is trusted"
            );
            false
        }
    };
    CodeSignature::Identified {
        trusted,
        app_sandbox,
    }
}

/// [`inspect`] and [`decide`] in one step.
pub fn classify(path: &Path, trust: &TrustRequirement) -> MacLaunchMode {
    decide(&inspect(path, trust))
}

/// Check that the process `pid` — normally spawned suspended — is running
/// code that validly satisfies `trust`'s identity requirement, is not
/// ad-hoc, and enables App Sandbox.
pub fn verify_process(pid: libc::pid_t, trust: &TrustRequirement) -> Result<(), String> {
    // SAFETY: an immutable framework constant initialised at load.
    let key = unsafe { kSecGuestAttributePid };
    // SAFETY: `pid` outlives the call; `CFNumberCreate` copies it.
    let number = Cf::new(unsafe {
        CFNumberCreate(
            std::ptr::null(),
            K_CF_NUMBER_SINT32,
            (&raw const pid).cast::<c_void>(),
        )
    })
    .ok_or("could not build the guest pid")?;
    let keys = [key];
    let values = [number.0];
    // SAFETY: one key and one value, both live CF objects, retained by the
    // dictionary through the standard CFType callbacks.
    let attributes = Cf::new(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    })
    .ok_or("could not build the guest attributes")?;
    let mut raw: CFTypeRef = std::ptr::null();
    // SAFETY: a null host means the system; `raw` receives an owned SecCode.
    let status =
        unsafe { SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes.0, 0, &raw mut raw) };
    if status != 0 {
        return Err(format!(
            "the running process has no code identity ({status})"
        ));
    }
    let code = Cf::new(raw).ok_or("the running process has no code identity")?;
    let req = requirement(&trust.identity_requirement())
        .map_err(|status| format!("the identity requirement does not compile ({status})"))?;
    // SAFETY: both references are live for the call.
    let status = unsafe { SecCodeCheckValidity(code.0, STRICT_DYNAMIC, req.0) };
    if status != 0 {
        return Err(format!(
            "the running process does not satisfy the trust requirement ({status})"
        ));
    }
    let info = signing_information(
        &code,
        K_SEC_CS_SIGNING_INFORMATION
            | K_SEC_CS_REQUIREMENT_INFORMATION
            | K_SEC_CS_DYNAMIC_INFORMATION,
    )
    .map_err(|status| {
        format!("the running process's signing information is unavailable ({status})")
    })?;
    if info.ad_hoc {
        return Err("the running process is ad-hoc signed".to_string());
    }
    if !info.app_sandbox {
        return Err(format!(
            "the running process's signature does not enable {APP_SANDBOX_ENTITLEMENT}"
        ));
    }
    Ok(())
}

/// Ad-hoc sign an unsigned executable in place. Apple silicon refuses to
/// exec code with no signature at all; ad-hoc is the least that runs, and it
/// earns nothing in [`decide`].
pub fn adhoc_sign(path: &Path) -> std::io::Result<()> {
    let output = Command::new("/usr/bin/codesign")
        .args(["--sign", "-", "--"])
        .arg(path)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "codesign --sign - {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

struct SigningInformation {
    ad_hoc: bool,
    app_sandbox: bool,
}

fn signing_information(code: &Cf, flags: SecCSFlags) -> Result<SigningInformation, OSStatus> {
    let mut raw: CFTypeRef = std::ptr::null();
    // SAFETY: `code` is a live SecCode/SecStaticCode; `raw` receives an
    // owned dictionary.
    let status = unsafe { SecCodeCopySigningInformation(code.0, flags, &raw mut raw) };
    if status != 0 {
        return Err(status);
    }
    let info = Cf::new(raw).ok_or(-1)?;
    // SAFETY: `info` is a live dictionary; the key constants are Security
    // framework statics; returned values are borrowed from `info`.
    unsafe {
        let flags_value = CFDictionaryGetValue(info.0, kSecCodeInfoFlags);
        let mut code_flags: i64 = 0;
        let ad_hoc = if !flags_value.is_null()
            && CFGetTypeID(flags_value) == CFNumberGetTypeID()
            && CFNumberGetValue(
                flags_value,
                K_CF_NUMBER_SINT64,
                (&raw mut code_flags).cast(),
            ) != 0
        {
            code_flags & K_SEC_CODE_SIGNATURE_ADHOC != 0
        } else {
            // A signature whose flags cannot be read is treated as naming no
            // signer.
            true
        };
        let entitlements = CFDictionaryGetValue(info.0, kSecCodeInfoEntitlementsDict);
        let app_sandbox = !entitlements.is_null()
            && CFGetTypeID(entitlements) == CFDictionaryGetTypeID()
            && is_true(entitlements, APP_SANDBOX_ENTITLEMENT);
        Ok(SigningInformation {
            ad_hoc,
            app_sandbox,
        })
    }
}

/// Whether `dict[key]` is the boolean `true` — not a string, not a number.
///
/// # Safety
///
/// `dict` must be a live `CFDictionary`.
unsafe fn is_true(dict: CFTypeRef, key: &str) -> bool {
    let Some(key) = cf_string(key) else {
        return false;
    };
    // SAFETY: the caller guarantees `dict`; `key` is live; the value is
    // borrowed from `dict`.
    unsafe {
        let value = CFDictionaryGetValue(dict, key.0);
        !value.is_null()
            && CFGetTypeID(value) == CFBooleanGetTypeID()
            && CFBooleanGetValue(value) != 0
    }
}

fn unreadable(reason: &str) -> CodeSignature {
    CodeSignature::Unreadable {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::{SeatbeltReason, TeamId};

    const TEXTEDIT: &str = "/System/Applications/TextEdit.app";
    const APPLE: TrustRequirement = TrustRequirement::Custom("anchor apple");

    #[test]
    fn every_production_requirement_compiles() {
        let team = TrustRequirement::DeveloperIdTeam(TeamId::new("ABCDE12345").unwrap());
        for trust in [TrustRequirement::DeveloperId, team] {
            for text in [trust.static_requirement(), trust.identity_requirement()] {
                assert!(requirement(&text).is_ok(), "{text} does not compile");
            }
        }
    }

    #[test]
    fn a_trusted_signature_enabling_app_sandbox_is_app_sandbox() {
        assert_eq!(
            inspect(Path::new(TEXTEDIT), &APPLE),
            CodeSignature::Identified {
                trusted: true,
                app_sandbox: true
            }
        );
        assert_eq!(
            classify(Path::new(TEXTEDIT), &APPLE),
            MacLaunchMode::AppSandbox
        );
    }

    #[test]
    fn a_trusted_signature_without_the_entitlement_is_seatbelt() {
        assert_eq!(
            inspect(Path::new("/usr/bin/true"), &APPLE),
            CodeSignature::Identified {
                trusted: true,
                app_sandbox: false
            }
        );
        assert_eq!(
            classify(Path::new("/usr/bin/true"), &APPLE),
            MacLaunchMode::Seatbelt(SeatbeltReason::NoAppSandboxEntitlement)
        );
    }

    #[test]
    fn a_sandboxed_signature_from_outside_developer_id_is_seatbelt() {
        // Apple's own apps are not Developer ID signed.
        assert_eq!(
            classify(Path::new(TEXTEDIT), &TrustRequirement::DeveloperId),
            MacLaunchMode::Seatbelt(SeatbeltReason::UntrustedSigner)
        );
    }

    #[test]
    fn an_unreadable_path_is_seatbelt() {
        let missing = Path::new("/nonexistent/tilo/app");
        assert!(matches!(
            inspect(missing, &TrustRequirement::DeveloperId),
            CodeSignature::Unreadable { .. }
        ));
        assert_eq!(
            classify(missing, &TrustRequirement::DeveloperId),
            MacLaunchMode::Seatbelt(SeatbeltReason::Unreadable)
        );
    }
}
