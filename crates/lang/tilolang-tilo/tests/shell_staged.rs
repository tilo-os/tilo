//! The shell's package as `tilolang check` builds it, its scenarios and the packages they stage
//! included, over a host that runs it: what a test stages one of the shell's scenarios in. The
//! shell a session runs leaves its scenarios out (`tilolang_tilo::shell::compile`), so a test of
//! one builds the package from its directory.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, Value};
use tilolang_tilo::host::Host;

pub(crate) const SHELL: &str = "so.tilo.shell";

/// What the shell's views called, in order.
#[derive(Clone, Default)]
pub(crate) struct Called(pub(crate) Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Called {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

pub(crate) fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The shell's package and the packages it uses, built once from `apps/tl/shell`, with the
/// platform's and the native apps' descriptions they compile against.
fn built() -> &'static (Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>) {
    static BUILT: OnceLock<(Vec<tilolang::Compiled>, Vec<tilolang::PackageMeta>)> =
        OnceLock::new();
    BUILT.get_or_init(|| {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/shell");
        let (built, published) =
            tilolang_tilo::catalogue::compile(&dir, &[], &mut tilolang::SeededEntropy(0))
                .unwrap_or_else(|why| panic!("the shell and its scenarios compile: {why}"));
        (
            built.into_iter().map(|package| package.compiled).collect(),
            published,
        )
    })
}

/// The shell over a host that runs it, the packages its scenarios stage loaded beside it, staged
/// as its scenario `scenario`, with each asked record in `asked` (`studio/invocations/alarm-2`)
/// publishing its `resolve`, as the session publishes the verbs of the workspace's records; what
/// its views call is told to the [`Called`].
pub(crate) fn staged(scenario: &str, asked: &[&str]) -> (Host, Called) {
    let (built, published) = built();
    let host = Host::new(ada(), || None).unwrap();
    for meta in published {
        host.runtime()
            .load_foreign(meta.clone())
            .unwrap_or_else(|why| panic!("{}: {why}", meta.app));
    }
    for compiled in built {
        host.load_compiled(compiled)
            .unwrap_or_else(|why| panic!("{}: {why}", compiled.meta.app));
    }
    let called = Called::default();
    host.runtime().set_foreign_invoker(called.clone());
    let mut published = tilolang_runtime::published::Published::default();
    for record in asked {
        published.insert(
            *record,
            "resolve",
            tilolang_runtime::published::PublishedAction {
                app: "tilo.invocations".to_owned(),
                // The host's `resolve`, in the order it declares its arguments.
                params: ["resolution", "choice", "chosen", "typed", "files", "edits"]
                    .map(str::to_owned)
                    .to_vec(),
            },
        );
    }
    host.runtime().set_published_actions(published);
    host.root_of(SHELL).unwrap();
    let action = host
        .package(SHELL)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|staged| staged.name == scenario)
        .unwrap_or_else(|| panic!("the shell stages {scenario:?}"))
        .action
        .clone();
    host.runtime()
        .invoke_action(&ada(), (action.as_str(), vec![]))
        .unwrap();
    (host, called)
}
