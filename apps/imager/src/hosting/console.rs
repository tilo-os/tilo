//! The action frames a console caller sends the installer's windows, on Unix: the resource,
//! every window's effects and rows, answered on the UI thread like a click.

use console_control::{Asked, ConsoleServer};
use tilo_ui_base::{widget::AppModel, widget::ModelPath, widget::Widget};
use ui::tilo_actions::remote::{
    ActionCallError, CataloguePage, Completer, InvocationReceipt, ReceiptStatus,
};
use ui::tilo_actions::{ActionFailure, ActionOutput, InvocationId};

use super::effect::Answer;
use super::{Reply, Store, invoke, published, state};

/// A console caller waiting for an effect that answers later.
pub(crate) struct Waiting {
    invocation: InvocationId,
    answer: Completer<Result<InvocationReceipt, ActionCallError>>,
}

impl Waiting {
    pub(super) fn finish(self, answer: Result<ActionOutput, String>) {
        let status = match answer {
            Ok(output) => ReceiptStatus::Completed { output },
            Err(message) => ReceiptStatus::Failed {
                error: ActionFailure {
                    code: "action_failed".to_owned(),
                    message,
                    details: None,
                    retryable: false,
                },
            },
        };
        self.answer.complete(Ok(InvocationReceipt {
            invocation: self.invocation,
            status,
        }));
    }
}

/// The window tree, answering the console between frames.
pub(super) struct Console {
    root: Box<dyn Widget>,
    store: Store,
    server: ConsoleServer,
    provider: &'static str,
}

impl Console {
    /// `root` answering the frames its console endpoints receive; each wakes the window
    /// through `wake`.
    pub(super) fn serve(
        root: Box<dyn Widget>,
        store: Store,
        provider: &'static str,
        wake: ui::WakeHandle,
    ) -> Self {
        let server = ConsoleServer::new(provider);
        server.serve_discovered(std::sync::Arc::new(move || wake.wake()));
        Self {
            root,
            store,
            server,
            provider,
        }
    }

    fn answer(&mut self) -> bool {
        let asked = self.server.take_pending();
        let answered = !asked.is_empty();
        for asked in asked {
            match asked {
                Asked::Catalogue { query, answer } => {
                    let resources = [published(&self.store, &mut *self.root)];
                    answer.complete(Ok(CataloguePage::published(
                        self.provider,
                        &resources,
                        &query,
                    )));
                }
                Asked::State { answer } => answer.complete(state(&mut *self.root)),
                Asked::Invoke { request, answer } => {
                    let resources = [published(&self.store, &mut *self.root)];
                    match console_control::admit_to(self.provider, &resources, &request) {
                        Err(error) => answer.complete(Ok(InvocationReceipt {
                            invocation: request.invocation,
                            status: ReceiptStatus::Rejected { error },
                        })),
                        Ok(encoded) => {
                            let reply = Reply::new(Answer::Console(Waiting {
                                invocation: request.invocation,
                                answer,
                            }));
                            invoke(&self.store, &mut *self.root, &encoded, reply);
                        }
                    }
                }
            }
        }
        answered
    }
}

impl tilo_ui_base::widget::Decorator for Console {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &*self.root
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut *self.root
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.answer();
        self.root.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let answered = self.answer();
        self.root.update(state, path) || answered
    }
}
