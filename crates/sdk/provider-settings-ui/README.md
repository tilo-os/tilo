# Provider settings UI

One portable provider-administration controller and SDK Form/Field panel, used by
Personal Settings and the standalone `so.tilo.model-providers` application.

`ProviderSettingsApp` is a resource-rooted framework application. Browser hosts
lend `ui::models::ProviderAdministration`; native launchers can call
`ProviderSettingsApp.init()` before mounting to capture the launcher's separate
administration grant. No model-selection capability is required.

`ProviderPanel<H>` owns the asynchronous controller, authoritative snapshot,
credential statuses, private input buffers, controls and published provider/model
rows. `PanelBindings` adapts those controls to the host's real action door.
Personal Settings retypes bindings and effects without changing its existing
action ids or parameters; it retains local model lifecycle, hardware controls and
the explicitly labelled offline metadata persistence adapter. The standalone app
has no offline settings mirror and starts with an empty configuration until the
backend answers.

A provider is added from a known one (`agent_provider_config::presets`), which
fills its endpoint and protocol. **Test connection** lists the provider's
models through the backend (`ProviderAdmin::list_models`) and offers them in a
chooser beside the typed model field. Sign-ins run where a browser and the
device vault are at hand: the native host adds them (Personal Settings,
`agent_sign_in` and `agent_vertex`).

Mutations use the backend revision. Conflicts reload authority and retain drafts
for an explicit retry. Credential input never belongs to a resource or settings
record; acknowledged submissions clear only the unchanged submitted draft.
Outstanding futures retain a scoped waker and keep animation frames requested.

The shared mounted contract in `src/mounted_tests.rs` is run through both
production hosts, with host-specific adapters for their action root and private
storage assertions. The production SDK dependency disables native default features,
so the panel and standalone definition compile in isolation for WebAssembly. Only
the mounted native tests enable the SDK's complete native test driver.
