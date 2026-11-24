//! What the shell opened an app with (`tool.context`): a terminal opened at a folder reads that
//! folder from the launch's tool context, and an app opened on its own reads nothing.

#[cfg(test)]
mod cases {
    use crate::common::*;
    use tilolang_runtime::files::FilesHost;
    use tilolang_runtime::{AmStore, ResourceRef, Runtime, SimpleAuthority, Value};

    const OPENED: &str = r#"app so.tilo.opened "Opened"

type Opened {
    /// The tool context the shell gave this app, as JSON; empty when it was opened on its own.
    context: Text
    /// The folder the context names, when this is a terminal opened at one.
    folder: Text
}

root Opened

default view opened for Opened

view opened(o: Opened) {
    text(o.context)
    text(o.folder)
}

action look(o: Opened) {
    o.context = tool.context()
    match json.decode<Launch>(o.context) {
        .ok(launch) => {
            if let directory = launch.directory {
                o.folder = directory
            }
        },
        .err(why) => { }
    }
}

type Launch {
    tool: Text
    directory: Option<Text>
}
"#;

    /// A device whose app was opened with `context`.
    #[derive(Clone, Default)]
    struct Device {
        context: String,
    }

    impl FilesHost for Device {
        fn list(&self, path: &str) -> Result<Vec<tilolang_runtime::files::Entry>, String> {
            Err(format!("`{path}` is not a folder on this device"))
        }

        fn stat(&self, _path: &str) -> Result<Option<tilolang_runtime::files::Entry>, String> {
            Ok(None)
        }

        fn read(&self, path: &str) -> Result<Vec<u8>, String> {
            Err(format!("`{path}` is not read here"))
        }

        fn watch(&self, _path: &str, _run: &str) -> Result<String, String> {
            Err("no watch".to_owned())
        }

        fn launch_context(&self) -> String {
            self.context.clone()
        }
    }

    struct Opened {
        rt: Runtime,
        app: App,
        o: ResourceRef,
    }

    impl Opened {
        fn on(device: Device) -> Self {
            let rt = Runtime::new(AmStore::new(), SimpleAuthority::new()).unwrap();
            rt.set_device_files(device);
            let app = App::compile(OPENED, &[]);
            app.load(&rt);
            let o = rt.create_root("Opened").unwrap();
            Self { rt, app, o }
        }

        fn look(&self) {
            let id = self.app.action("look");
            self.rt
                .invoke_action(&me(), (id.as_str(), vec![Value::resource(&self.o)]))
                .unwrap_or_else(|error| panic!("look: {error:?}"));
        }

        fn field(&self, name: &str) -> String {
            match self.rt.read(&self.o, name).unwrap() {
                Value::Text(text) => text,
                other => panic!("{name} is text, not {other:?}"),
            }
        }
    }

    #[test]
    fn a_terminal_opened_at_a_folder_reads_that_folder_from_its_launch_context() {
        let context = r#"{"tool":"terminal","directory":"/home/ada/site"}"#;
        let opened = Opened::on(Device {
            context: context.into(),
        });
        opened.look();
        assert_eq!(opened.field("context"), context);
        assert_eq!(opened.field("folder"), "/home/ada/site");
    }

    #[test]
    fn an_app_opened_on_its_own_reads_empty_text() {
        let opened = Opened::on(Device::default());
        opened.look();
        assert_eq!(opened.field("context"), "");
        assert_eq!(opened.field("folder"), "");
    }
}
