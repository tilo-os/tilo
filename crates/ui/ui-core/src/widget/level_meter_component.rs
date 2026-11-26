//! [`LevelMeter`] as a described component.

use tilo_ui_base::{component::Component, component::Described};

use super::LevelMeter;

/// How loud an audio input is right now, as a bar that follows the sound and keeps a falling peak:
/// the component opens the device's meter itself while it runs.
#[derive(Component, Default)]
#[component(name = "LevelMeter", role = "ProgressBar", label = "label")]
pub struct LevelMeterProps {
    /// The audio device's name, as the sound resource publishes it (the node name).
    #[component(required, data)]
    pub source: String,
    /// Whether the source is a sink to meter (the volume a speaker plays) rather than a
    /// microphone to read.
    pub output: bool,
    /// Whether the meter is being read now: it opens only while this is true.
    pub running: bool,
    /// Its spoken name.
    pub label: Option<String>,
}

impl Described for LevelMeter {
    type Props = LevelMeterProps;

    fn from_props(_key: &str, props: LevelMeterProps) -> Self {
        let LevelMeterProps {
            source,
            output,
            running,
            label,
        } = props;
        let mut meter = Self::new(source, false);
        meter.set_output(output);
        if let Some(label) = label {
            meter.set_label(Some(label));
        }
        meter.set_running(running);
        meter
    }

    fn set_props(&mut self, props: LevelMeterProps) -> bool {
        self.set_label(props.label);
        self.set_source(props.source);
        self.set_output(props.output);
        self.set_running(props.running);
        true
    }
}
