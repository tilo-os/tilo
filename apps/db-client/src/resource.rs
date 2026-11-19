//! The app on its resource: `db-client-model` keeps the model and its verbs;
//! here the app draws it, and the sample database development sessions show.

#[cfg(feature = "samples")]
use ui::framework::Fixture;
use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
#[cfg(feature = "samples")]
use ui::tilo_resource::Invoke;
use ui::widget::Element;
#[cfg(feature = "samples")]
use uuid::Uuid;

use crate::app::DbClientApp;
#[cfg(feature = "samples")]
use crate::model::{ConnectionProfile, Engine};
#[cfg(feature = "samples")]
use crate::saved::{SavedConnection, id_of};

pub use db_client_model::{DbEffect, Workspace, workspace};

#[cfg(feature = "samples")]
pub(crate) struct SampleDatabase {
    pub(crate) connection: Uuid,
}

#[cfg(feature = "samples")]
const SAMPLE_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0001);
#[cfg(feature = "samples")]
const ANALYTICS_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0002);
#[cfg(feature = "samples")]
const SHOP_CONNECTION: Uuid = Uuid::from_u128(0x5a3b_1e00_0000_4000_8000_0000_0000_0003);

#[cfg(feature = "samples")]
fn sample_connections() -> Vec<ConnectionProfile> {
    let mut sample = ConnectionProfile::sqlite_sample();
    sample.id = SAMPLE_CONNECTION;
    let mut analytics = ConnectionProfile::new("Production analytics", Engine::Postgres);
    analytics.id = ANALYTICS_CONNECTION;
    analytics.host = "db.internal".into();
    analytics.database = "analytics".into();
    analytics.user = "reader".into();
    let mut shop = ConnectionProfile::new("Staging shop", Engine::MySql);
    shop.id = SHOP_CONNECTION;
    shop.host = "mysql.staging".into();
    shop.database = "shop".into();
    shop.user = "app".into();
    vec![sample, analytics, shop]
}

pub struct DbClientShell;

impl ResourceApp for DbClientShell {
    type Resource = Workspace;
    const ID: &'static str = ui::apps::DB_CLIENT;
    const TITLE: &'static str = "Database Client";
    const TITLE_KEY: &'static str = "db-client-app-title";
    const ICON: Option<&'static str> = Some("db-client");

    fn view(cx: &mut ViewCx<Workspace>) -> Element {
        let app = DbClientApp::with_actions(&cx.actions());
        let (opener, index) = {
            let state = app.state();
            (state.opener(), state.search_index())
        };
        // A search hit, or an agent's `open`, shows the item it names.
        cx.on_open_item(opener);
        // A catalogue hit — a relation, column, result, transaction or query
        // of the open connection — runs what its row offers.
        let dispatcher = cx.dispatcher();
        cx.on_open(
            move |anchor: &str| -> Result<(), ui::tilo_resource::Refusal> {
                let effect = index.opened(anchor).ok_or_else(|| {
                    ui::tilo_resource::Refusal::new(format!(
                        "`{anchor}` is not in the open catalogue"
                    ))
                })?;
                dispatcher.effect(&effect);
                Ok(())
            },
        );
        crate::app::window(cx, app)
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(crate::i18n::catalogs())
    }

    /// `empty` is the first run; every other scenario shows the bundled
    /// sample catalog with a query run against it, on the tab or dialog it
    /// names (see `Store::stage_scenario` in the app).
    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        &[
            "data",
            "structure",
            "wide",
            "connect",
            "confirm",
            "error",
            "empty",
        ]
    }

    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut Fixture<Workspace>) {
        if fixture.scenario() != Some("empty") {
            fixture.provide(SampleDatabase {
                connection: SAMPLE_CONNECTION,
            });
        }
    }

    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Workspace>> {
        if name == Some("empty") {
            return Vec::new();
        }
        let paths = Workspace::paths();
        sample_connections()
            .iter()
            .enumerate()
            .map(|(at, profile)| {
                Invoke::edit(&paths.connections.insert_as(
                    &id_of(profile.id),
                    at,
                    &SavedConnection::of(profile),
                ))
            })
            .chain([Invoke::edit(
                &paths.sql.replace(crate::app::scenario_query(name)),
            )])
            .filter_map(Result::ok)
            .collect()
    }
}

#[cfg(test)]
mod tests;

ui::reach_tests!(DbClientShell);

#[cfg(test)]
mod agent_disclosure_tests {
    use ui::framework::ResourceApp;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::ResourceSpec;

    /// The document read of the first resource of `kind` any scenario
    /// publishes, invoked as `tilo_read` on it does.
    fn read_first(kind: &str) -> ui::serde_json::Value {
        for scenario in <super::DbClientShell as ResourceApp>::scenarios() {
            let mut app = TestApp::<super::DbClientShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            app.settle();
            let resources = app.harness().resources();
            if let Some((resource, read)) = document_read(&resources, kind) {
                return app
                    .call_on(&resource, &read, ui::serde_json::json!({}))
                    .unwrap_or_else(|error| panic!("{kind} in {scenario}: {error}"))
                    .unwrap_or_else(|| panic!("the read of {kind} answers"));
            }
        }
        panic!("no scenario publishes a {kind} with a document read");
    }

    fn document_read(resources: &[ResourceSpec], kind: &str) -> Option<(String, String)> {
        resources.iter().find_map(|resource| {
            resource
                .actions
                .iter()
                .find(|action| resource.kind == kind && action.contract.reads_document())
                .map(|read| (resource.id.as_str().to_owned(), read.id.clone()))
                .or_else(|| document_read(&resource.children, kind))
        })
    }

    #[test]
    fn a_query_result_is_read_through_its_document_read() {
        let answer = read_first("database.query-result");
        assert!(answer["message"].is_string(), "{answer}");
        assert!(answer["returns_rows"].is_boolean(), "{answer}");
    }
}
