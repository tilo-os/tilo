//! Native Windows notifications for installed desktop apps.
//!
//! The installer must register a Start menu shortcut with the same app ID.
//! Notification settings remain under Windows/user control. Successful submission
//! does not imply that Windows displayed a banner (e.g. Do Not Disturb).

#![allow(
    clippy::missing_errors_doc,
    clippy::arithmetic_side_effects,
    clippy::needless_pass_by_value,
    clippy::cmp_owned,
    clippy::borrow_as_ptr,
    reason = "moved unchanged from tilo-host's Windows toasts, which no Windows lint lane covers yet"
)]

use std::{marker::PhantomData, path::Path, rc::Rc, sync::OnceLock};

use tilo_clock::{Duration, SystemTime, UNIX_EPOCH};
use windows::Data::Xml::Dom::XmlDocument;
use windows::UI::Notifications::{
    NotificationSetting, ScheduledToastNotification, ToastNotification, ToastNotificationManager,
    ToastNotifier,
};
use windows::Win32::Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoIncrementMTAUsage, IPersistFile, STGM_READWRITE,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::System::WinRT::{RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{IShellLinkW, SHStrDupW, ShellLink};
use windows::core::{GUID, HSTRING, Interface, PCWSTR};

struct Apartment {
    acquired: bool,
    // Initialization must be balanced on its original thread.
    _thread: PhantomData<Rc<()>>,
}

impl Apartment {
    fn new() -> windows::core::Result<Self> {
        // windows-core caches agile activation factories for the process lifetime.
        // Keep the COM runtime alive for that same lifetime, even when the final
        // thread-local STA guard goes away between notifications. This matches
        // windows-core's implicit CoIncrementMTAUsage initialization path.
        static RUNTIME: OnceLock<Result<(), windows::core::HRESULT>> = OnceLock::new();
        let initialized = RUNTIME.get_or_init(|| {
            // SAFETY: once per process. The opaque usage cookie is intentionally
            // retained until process exit, just like the binding's factory cache.
            unsafe { CoIncrementMTAUsage() }
                .map(|_cookie| ())
                .map_err(|error| error.code())
        });
        if let Err(code) = initialized {
            return Err(windows::core::Error::from_hresult(*code));
        }
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
    activation_uri: Option<String>,
    // Drop COM interfaces before releasing the apartment.
    _apartment: Apartment,
}

/// A pending notification owned by this application, as reported by Windows.
#[derive(Debug)]
pub struct ScheduledNotice {
    pub id: String,
    pub delivery_time: SystemTime,
}

impl HostNotifications {
    pub fn new(app_id: &str) -> windows::core::Result<Self> {
        let apartment = Apartment::new()?;
        let app_id = HSTRING::from(app_id);
        let notifier = ToastNotificationManager::CreateToastNotifierWithId(&app_id)?;
        Ok(Self {
            notifier,
            app_id,
            activation_uri: None,
            _apartment: apartment,
        })
    }

    /// Let Windows open the application's registered URI when an alert is
    /// activated, including after the submitting process has exited.
    pub fn with_protocol_activation(mut self, uri: &str) -> windows::core::Result<Self> {
        let uri = url::Url::parse(uri).map_err(|_| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
        })?;
        self.activation_uri = Some(uri.into());
        Ok(self)
    }

    fn notification_xml(&self, title: &str, body: &str) -> windows::core::Result<XmlDocument> {
        let activation = self
            .activation_uri
            .as_deref()
            .map_or_else(String::new, |uri| {
                format!(
                    " activationType=\"protocol\" launch=\"{}\"",
                    escape_xml(uri)
                )
            });
        let xml = XmlDocument::new()?;
        xml.LoadXml(&HSTRING::from(format!(
            "<toast{activation}><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>",
            escape_xml(title), escape_xml(body)
        )))?;
        Ok(xml)
    }

    /// A stable tag replaces a previous submission instead of duplicating it.
    /// Windows allows at most 16 UTF-16 code units per tag.
    pub fn show(&self, tag: &str, title: &str, body: &str) -> windows::core::Result<()> {
        if tag.is_empty() || tag.encode_utf16().count() > 16 {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let xml = self.notification_xml(title, body)?;
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

    /// Persist a one-time notification in Windows' scheduler. An identical
    /// request is a no-op; changing its time or contents replaces that ID.
    /// IDs allow 15 UTF-16 units: this Windows host counts the terminator in
    /// the documented 16-character limit (unlike ordinary toast tags).
    pub fn schedule(
        &self,
        id: &str,
        title: &str,
        body: &str,
        delivery_time: SystemTime,
    ) -> windows::core::Result<()> {
        if id.is_empty() || id.encode_utf16().count() > 15 {
            return Err(windows::core::Error::from_hresult(
                windows::Win32::Foundation::E_INVALIDARG,
            ));
        }
        let delivery = to_windows_time(delivery_time)?;
        let xml = self.notification_xml(title, body)?;
        let expected_xml = xml.GetXml()?;
        for pending in self.pending()? {
            if pending.Id()?.to_string() == id {
                if pending.DeliveryTime()? == delivery
                    && pending.Content()?.GetXml()? == expected_xml
                {
                    return Ok(());
                }
                self.notifier.RemoveFromSchedule(&pending)?;
            }
        }
        let toast = ScheduledToastNotification::CreateScheduledToastNotification(&xml, delivery)?;
        toast.SetId(&HSTRING::from(id))?;
        toast.SetTag(&HSTRING::from(id))?;
        toast.SetGroup(&HSTRING::from("tilo"))?;
        self.notifier.AddToSchedule(&toast)
    }

    pub fn scheduled(&self) -> windows::core::Result<Vec<ScheduledNotice>> {
        self.pending()?
            .into_iter()
            .map(|notice| {
                Ok(ScheduledNotice {
                    id: notice.Id()?.to_string(),
                    delivery_time: from_windows_time(notice.DeliveryTime()?)?,
                })
            })
            .collect()
    }

    pub fn cancel_schedule(&self, id: &str) -> windows::core::Result<()> {
        for notice in self.pending()? {
            if notice.Id()?.to_string() == id {
                self.notifier.RemoveFromSchedule(&notice)?;
            }
        }
        Ok(())
    }

    fn pending(&self) -> windows::core::Result<Vec<ScheduledToastNotification>> {
        match self.notifier.Setting() {
            Ok(_) => {}
            Err(error)
                if error.code() == windows::Win32::Foundation::ERROR_NOT_FOUND.to_hresult() =>
            {
                // Unpackaged apps must submit once before using the scheduler.
                // Register silently, with an expiry as a backstop if removal fails.
                let xml = XmlDocument::new()?;
                xml.LoadXml(&HSTRING::from("<toast><visual><binding template=\"ToastGeneric\"><text>Tilo</text></binding></visual></toast>"))?;
                let toast = ToastNotification::CreateToastNotification(&xml)?;
                toast.SetTag(&HSTRING::from("tilo-register"))?;
                toast.SetGroup(&HSTRING::from("tilo"))?;
                toast.SetSuppressPopup(true)?;
                let expiry: windows::Foundation::IReference<windows::Foundation::DateTime> =
                    windows::Foundation::PropertyValue::CreateDateTime(to_windows_time(
                        tilo_clock::wall() + Duration::from_secs(15),
                    )?)?
                    .cast()?;
                toast.SetExpirationTime(&expiry)?;
                self.notifier.Show(&toast)?;
                self.remove("tilo-register")?;
            }
            Err(error) => return Err(error),
        }
        Ok(self
            .notifier
            .GetScheduledToastNotifications()?
            .into_iter()
            .collect())
    }
}

/// The Windows [`crate::Notifier`]: toasts under the installer-registered
/// app id. Each call opens its own notifier: the COM apartment it needs is
/// bound to the calling thread.
pub(crate) struct Toasts {
    app_id: String,
}

impl Toasts {
    pub(crate) fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
        }
    }

    fn host(&self) -> Result<HostNotifications, crate::NotifyError> {
        HostNotifications::new(&self.app_id).map_err(failed)
    }
}

fn failed(error: windows::core::Error) -> crate::NotifyError {
    crate::NotifyError::Failed(error.to_string())
}

impl crate::Notifier for Toasts {
    fn show(&self, notification: &crate::Notification) -> Result<(), crate::NotifyError> {
        crate::validate(notification)?;
        self.host()?
            .show(&notification.tag, &notification.title, &notification.body)
            .map_err(failed)
    }

    fn withdraw(&self, tag: &str) -> Result<(), crate::NotifyError> {
        crate::validate_tag(tag)?;
        self.host()?.remove(tag).map_err(failed)
    }

    fn schedule(
        &self,
        notification: &crate::Notification,
        at: SystemTime,
    ) -> Result<(), crate::NotifyError> {
        crate::validate(notification)?;
        self.host()?
            .schedule(
                &notification.tag,
                &notification.title,
                &notification.body,
                at,
            )
            .map_err(failed)
    }

    fn scheduled(&self) -> Result<Vec<crate::Scheduled>, crate::NotifyError> {
        Ok(self
            .host()?
            .scheduled()
            .map_err(failed)?
            .into_iter()
            .map(|notice| crate::Scheduled {
                tag: notice.id,
                at: notice.delivery_time,
            })
            .collect())
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), crate::NotifyError> {
        crate::validate_tag(tag)?;
        self.host()?.cancel_schedule(tag).map_err(failed)
    }
}

const WINDOWS_UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;

fn to_windows_time(time: SystemTime) -> windows::core::Result<windows::Foundation::DateTime> {
    let ticks = time
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos() / 100).ok())
        .and_then(|ticks| ticks.checked_add(WINDOWS_UNIX_EPOCH_TICKS))
        .ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
        })?;
    Ok(windows::Foundation::DateTime {
        UniversalTime: ticks,
    })
}

fn from_windows_time(time: windows::Foundation::DateTime) -> windows::core::Result<SystemTime> {
    let ticks = time
        .UniversalTime
        .checked_sub(WINDOWS_UNIX_EPOCH_TICKS)
        .and_then(|ticks| u64::try_from(ticks).ok())
        .ok_or_else(|| {
            windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG)
        })?;
    UNIX_EPOCH
        .checked_add(
            Duration::from_secs(ticks / 10_000_000)
                + Duration::from_nanos((ticks % 10_000_000) * 100),
        )
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG))
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
    // PROPVARIANT's Drop calls PropVariantClear. SHStrDupW allocates the string
    // with the matching COM allocator; never borrow an HSTRING's storage here.
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
        (*value.Anonymous.Anonymous).Anonymous.pwszVal = SHStrDupW(&app_id)?;
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
    fn notification_hosts_registrations_and_schedules_are_owned_and_exact() {
        // phase: notification_hosts_can_be_recreated_on_successive_threads
        {
            for _ in 0..4 {
                std::thread::spawn(|| {
                    let host = HostNotifications::new("so.tilo.clocks").unwrap();
                    let xml = host.notification_xml("Timer", "Ready").unwrap();
                    assert!(xml.GetXml().unwrap().to_string().contains("Ready"));
                })
                .join()
                .unwrap();
            }
        }
        // phase: repeated_shortcut_registration_owns_its_property_string
        {
            let directory = tempfile::tempdir().unwrap();
            let shortcut = directory.path().join("Clocks test.lnk");
            for id in [
                "so.tilo.test.first",
                "so.tilo.test.second",
                "so.tilo.test.third",
            ] {
                register_shortcut(
                    id,
                    "Tilo test",
                    &std::env::current_exe().unwrap(),
                    &shortcut,
                )
                .unwrap();
                let _apartment = Apartment::new().unwrap();
                // SAFETY: thread apartment initialized, all interfaces owned locally.
                unsafe {
                    let link: IShellLinkW =
                        CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
                    let file: IPersistFile = link.cast().unwrap();
                    file.Load(&HSTRING::from(shortcut.as_os_str()), STGM_READWRITE)
                        .unwrap();
                    let store: IPropertyStore = link.cast().unwrap();
                    let key = PROPERTYKEY {
                        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
                        pid: 5,
                    };
                    let value = store.GetValue(&key).unwrap();
                    assert_eq!(
                        windows::core::BSTR::try_from(&value).unwrap().to_string(),
                        id
                    );
                }
            }
        }
        // phase: scheduler_time_conversion_preserves_windows_precision
        {
            let time = UNIX_EPOCH + Duration::new(1_800_000_000, 123_456_700);
            assert_eq!(
                from_windows_time(to_windows_time(time).unwrap()).unwrap(),
                time
            );
            assert!(to_windows_time(UNIX_EPOCH - Duration::from_secs(1)).is_err());
            assert!(
                from_windows_time(windows::Foundation::DateTime {
                    UniversalTime: i64::MIN
                })
                .is_err()
            );
        }
    }

    #[test]
    fn notification_content_is_preserved_without_markup_or_xml_injection() {
        // phase: notification_activation_uri_is_preserved_without_xml_injection
        {
            let uri = "tilo-clocks:open?one=1&two=2";
            let host = HostNotifications::new("so.tilo.clocks")
                .unwrap()
                .with_protocol_activation(uri)
                .unwrap();
            let xml = host.notification_xml("Tea <ready>", "A & B").unwrap();
            let root = xml.DocumentElement().unwrap();
            assert_eq!(
                root.GetAttribute(&HSTRING::from("activationType"))
                    .unwrap()
                    .to_string(),
                "protocol"
            );
            assert_eq!(
                root.GetAttribute(&HSTRING::from("launch"))
                    .unwrap()
                    .to_string(),
                uri
            );
            assert_eq!(
                xml.GetElementsByTagName(&HSTRING::from("text"))
                    .unwrap()
                    .Length()
                    .unwrap(),
                2
            );
            // The failing builder consumes the host and releases its apartment.
            // Release the test's private COM inspection objects first.
            drop(root);
            drop(xml);
            assert!(host.with_protocol_activation("not a URI").is_err());
        }
        // phase: notification_text_cannot_add_markup
        {
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
}
