//! Native Windows notifications for installed desktop apps.
//!
//! The installer must register a Start menu shortcut with the same app ID.
//! Notification settings remain under Windows/user control. Successful submission
//! does not imply that Windows displayed a banner (e.g. Do Not Disturb).

use std::{marker::PhantomData, path::Path, rc::Rc};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::Win32::Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READWRITE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, PWSTR};

struct Apartment {
    acquired: bool,
    // Initialization must be balanced on its original thread.
    _thread: PhantomData<Rc<()>>,
}

impl Apartment {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: thread-bound guard balances each successful initialization.
        let result = unsafe { RoInitialize(RO_INIT_SINGLETHREADED) };
        let acquired = match result {
            Ok(()) => true,
            Err(error) if error.code() == RPC_E_CHANGED_MODE => false,
            Err(error) => return Err(error),
        };
        Ok(Self {
            acquired,
            _thread: PhantomData,
        })
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.acquired {
            // SAFETY: !Send guard, one uninitialize per successful initialize.
            unsafe { RoUninitialize() };
        }
    }
}

pub struct HostNotifications {
    notifier: ToastNotifier,
    app_id: HSTRING,
    // Drop COM interfaces before releasing the apartment.
    _apartment: Apartment,
}

impl HostNotifications {
    pub fn new(app_id: &str) -> windows::core::Result<Self> {
        let apartment = Apartment::new()?;
        let app_id = HSTRING::from(app_id);
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&app_id)?;
        Ok(Self {
            notifier,
            app_id,
            _apartment: apartment,
        })
    }

    /// A stable tag replaces a previous submission instead of duplicating it.
    /// Windows allows at most 16 UTF-16 code units per tag.
    pub fn show(&self, tag: &str, title: &str, body: &str) -> windows::core::Result<()> {
        if tag.is_empty() || tag.encode_utf16().count() > 16 {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            "<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape_xml(title), escape_xml(body)
        )))?;
        let toast = ToastNotification::CreateToastNotification(&xml)?;
        toast.SetTag(&HSTRING::from(tag))?;
        toast.SetGroup(&HSTRING::from("tilo"))?;
        self.notifier.Show(&toast)
    }

    /// Windows can return `ERROR_NOT_FOUND` before this app's first submission.
    /// Do not use this query as a prerequisite for `show`.
    pub fn enabled(&self) -> windows::core::Result<bool> {
        Ok(self.notifier.Setting()? == NotificationSetting::Enabled)
    }

    /// Inspect only this application's notification history.
    pub fn history(&self) -> windows::core::Result<Vec<(String, String)>> {
        let history = ToastNotificationManager::History()?.GetHistoryWithId(&self.app_id)?;
        history
            .into_iter()
            .map(|toast| {
                Ok((
                    toast.Tag()?.to_string(),
                    toast.Content()?.GetXml()?.to_string(),
                ))
            })
            .collect()
    }

    pub fn remove(&self, tag: &str) -> windows::core::Result<()> {
        ToastNotificationManager::History()?.RemoveGroupedTagWithId(
            &HSTRING::from(tag),
            &HSTRING::from("tilo"),
            &self.app_id,
        )
    }
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Install a shortcut using the same identity used by `HostNotifications`.
/// Called by the Windows installer, never as a side effect of showing a toast.
pub fn register_shortcut(
    app_id: &str,
    title: &str,
    executable: &Path,
    shortcut: &Path,
) -> windows::core::Result<()> {
    let _apartment = Apartment::new()?;
    let existing = shortcut.exists();
    let executable = HSTRING::from(executable.as_os_str());
    let shortcut = HSTRING::from(shortcut.as_os_str());
    let app_id = HSTRING::from(app_id);
    // SAFETY: initialized apartment; all referenced UTF-16 buffers outlive calls.
    // SetValue copies the borrowed PROPVARIANT string. It is not allocated with
    // CoTaskMemAlloc, so we deliberately do not call PropVariantClear on it.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        let file: IPersistFile = link.cast()?;
        if existing {
            // Preserve the installer's working directory, icon and other metadata.
            file.Load(PCWSTR(shortcut.as_ptr()), STGM_READWRITE)?;
        }
        link.SetPath(&executable)?;
        link.SetDescription(&HSTRING::from(title))?;
        let store: IPropertyStore = link.cast()?;
        let mut value = PROPVARIANT::default();
        (*value.Anonymous.Anonymous).vt = VT_LPWSTR;
        (*value.Anonymous.Anonymous).Anonymous.pwszVal = PWSTR(app_id.as_ptr().cast_mut());
        let key = PROPERTYKEY {
            fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
            pid: 5,
        };
        store.SetValue(&key, &value)?;
        store.Commit()?;
        file.Save(PCWSTR(shortcut.as_ptr()), true)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_cannot_add_markup() {
        let _apartment = Apartment::new().unwrap();
        let value = "Tea & <coffee> \"ready\" 'now'";
        let xml = XmlDocument::new().unwrap();
        xml.LoadXml(&HSTRING::from(format!(
            "<text>{}</text>",
            escape_xml(value)
        )))
        .unwrap();
        assert_eq!(
            xml.DocumentElement()
                .unwrap()
                .InnerText()
                .unwrap()
                .to_string(),
            value
        );
    }
}
