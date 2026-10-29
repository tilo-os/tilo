//! Messages' messaging service in the page: the person's Tilo messages,
//! carried by the private conversations of the room the page is in
//! (`tilo_messaging::room`), and the sign-up for their Tilo account where
//! they have none. The page lends them to each Messages window as a native
//! session lends its messaging host, so Messages sends and reads the same
//! way on both.
//!
//! The service is its own module, fetched beside Messages' own the first
//! time Messages opens (`crate::split::with_app`): the start module carries
//! only the lender.
//!
//! The page reaches no native messaging host, and holds no key of its own
//! for Signal, `WhatsApp` or SMS; those services stay on the person's native
//! devices.

use std::ffi::c_void;
use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;

use tilo_browser_runtime::Lend;
use tilo_ui::application::HostContext;

use crate::split::Callback;

/// Messages' app id.
const MESSAGES: &str = "so.tilo.messages";

/// The service's lender, once its module is in.
static SERVICE: OnceLock<Lend> = OnceLock::new();

#[link(wasm_import_module = "./__wasm_split.js")]
#[allow(
    improper_ctypes,
    reason = "the function pointer only passes between Rust"
)]
unsafe extern "C" {
    #[link_name = "__wasm_split_load_messaging_service_c41e9b7d2a865f03_code"]
    fn load_module(callback: Callback, data: *const c_void);
    #[link_name = "__wasm_split_00___messaging_service___00_import_c41e9b7d2a865f03_code"]
    fn code() -> Lend;
}

#[unsafe(export_name = "__wasm_split_00___messaging_service___00_export_c41e9b7d2a865f03_code")]
#[allow(
    improper_ctypes_definitions,
    reason = "the function pointer only passes between Rust"
)]
extern "C" fn export() -> Lend {
    service::lend
}

/// Lend each Messages window the page mounts its messaging service,
/// fetched whenever Messages loads.
pub(crate) fn register() {
    tilo_browser_runtime::lend_on_mount(MESSAGES, lend);
    crate::split::with_app(MESSAGES, fetch);
}

fn fetch() -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(async {
        if SERVICE.get().is_none() && crate::split::load(load_module).await {
            // SAFETY: the module that defines `code` is loaded.
            let _ = SERVICE.set(unsafe { code() });
        }
    })
}

/// A window of Messages mounted before the service could be fetched is
/// lent nothing and says so; the next one opened fetches it again.
fn lend(context: &mut HostContext) {
    if let Some(lend) = SERVICE.get() {
        lend(context);
    }
}

/// The service itself, reached only through [`export`].
mod service {
    use std::cell::RefCell;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use accounts::person::{ACCOUNTS_OWNER, PersonAccounts};
    use tilo_messaging::room::{PrivateAnswer, PrivateDoor, RoomEnrollment, RoomMessaging};
    use tilo_relay_protocol::rooms::private::PrivateOperation;
    use tilo_ui::application::HostContext;
    use tilo_ui::record::person::PersonRecords;

    thread_local! {
        /// The person's records of the last Messages window mounted: where a
        /// Tilo account signed up for in it is published.
        static RECORDS: RefCell<Option<PersonRecords>> = const { RefCell::new(None) };
    }

    /// The room the page is in, as the service's door.
    struct PageDoor;

    impl PrivateDoor for PageDoor {
        fn ask(&self, operation: PrivateOperation) -> PrivateAnswer {
            tilo_browser_runtime::ask_private(operation)
        }

        fn spawn(&self, work: Pin<Box<dyn Future<Output = ()> + Send>>) {
            wasm_bindgen_futures::spawn_local(work);
        }
    }

    pub(super) fn lend(context: &mut HostContext) {
        // A private window keeps nothing of the person's, and the page knows
        // no person before its peer certified one.
        if context.is_private() {
            return;
        }
        let Some(identity) = tilo_ui::local_identity::load() else {
            return;
        };
        let me = hex::encode(identity.group_public_key);
        let principal = context.author().0.clone();
        RECORDS.with_borrow_mut(|records| *records = context.person_records().cloned());
        context.lend(RoomMessaging::new(&me, Arc::new(PageDoor)).client(&principal));
        let shown = if identity.display_name.trim().is_empty() {
            me
        } else {
            identity.display_name
        };
        context.lend(RoomEnrollment::new(&shown, Arc::new(publish)).client(&principal));
    }

    /// Publish the person's Tilo account in their accounts record, as Settings
    /// keeps it, through the person's records the page holds.
    fn publish(id: &str, name: &str, scope: &str) -> Result<(), String> {
        let records = RECORDS
            .with_borrow(Clone::clone)
            .ok_or("the person's records are not open")?;
        let settings = ui::framework::SettingsRecords::of_person(
            ACCOUNTS_OWNER,
            "Settings",
            &[],
            records
                .of_package(ACCOUNTS_OWNER)
                .map_err(|error| error.to_string())?,
        );
        let accounts = settings
            .shared::<PersonAccounts>()
            .ok_or("the person's accounts are not open")?;
        accounts
            .invoke(&PersonAccounts::PUBLISH_TILO_ACCOUNT.with((
                id.to_owned(),
                name.to_owned(),
                scope.to_owned(),
                1,
            )))
            .map_err(|error| error.to_string())?;
        accounts.record().flush();
        Ok(())
    }
}
