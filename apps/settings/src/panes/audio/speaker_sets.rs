//! The person's speaker sets as outputs of Sound: read from the shell's speaker
//! lane, and chosen through it.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeakerSetOutput {
    pub id: String,
    pub name: String,
    pub speakers: usize,
    pub output_here: bool,
}

pub(super) struct SpeakerSets {
    pub shown: Vec<SpeakerSetOutput>,
    pub read: crate::device_read::DeviceRead<Vec<SpeakerSetOutput>>,
    pub due: bool,
}

impl Default for SpeakerSets {
    fn default() -> Self {
        Self {
            shown: Vec::new(),
            read: crate::device_read::DeviceRead::default(),
            due: true,
        }
    }
}

impl SpeakerSets {
    pub fn poll(
        &mut self,
        speakers: &std::sync::Arc<dyn crate::speaker_setup::SpeakerService>,
    ) -> bool {
        let read = if self.read.is_finished()
            && let Some(sets) = self.read.take()
        {
            sets
        } else {
            if !self.due || !self.read.is_finished() {
                return false;
            }
            self.due = false;
            let service = std::sync::Arc::clone(speakers);
            if self.read.start(move || read_speaker_sets(service.as_ref())) {
                return false;
            }
            read_speaker_sets(speakers.as_ref())
        };
        if read == self.shown {
            return false;
        }
        self.shown = read;
        true
    }
}

fn read_speaker_sets(service: &dyn crate::speaker_setup::SpeakerService) -> Vec<SpeakerSetOutput> {
    match service.call(ui::collaboration::SpeakerCommand::Snapshot) {
        Ok(ui::collaboration::SpeakerReply::Snapshot { snapshot }) if snapshot.media_enabled => {
            snapshot
                .groups
                .into_iter()
                .map(|group| SpeakerSetOutput {
                    id: group.id,
                    name: group.name,
                    speakers: group.members.len(),
                    output_here: group.output_here,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

pub(super) fn speaker_call(
    service: &dyn crate::speaker_setup::SpeakerService,
    command: ui::collaboration::SpeakerCommand,
) -> Result<(), String> {
    match service.call(command)? {
        ui::collaboration::SpeakerReply::Rejected { reason } => Err(reason),
        _ => Ok(()),
    }
}

/// The speaker sets a screenshot and the crate's tests show: one stereo
/// pair, not in use.
pub(super) struct SampleSpeakerSets;

impl crate::speaker_setup::SpeakerService for SampleSpeakerSets {
    fn call(
        &self,
        command: ui::collaboration::SpeakerCommand,
    ) -> Result<ui::collaboration::SpeakerReply, String> {
        use ui::collaboration::{SpeakerGroupInfo, SpeakerReply, SpeakerSnapshot};
        Ok(match command {
            ui::collaboration::SpeakerCommand::Snapshot => SpeakerReply::Snapshot {
                snapshot: SpeakerSnapshot {
                    media_enabled: true,
                    groups: vec![SpeakerGroupInfo {
                        id: "cd".repeat(16),
                        name: "Living room".into(),
                        leader_label: "Desk".into(),
                        led_here: true,
                        members: [
                            ("Desk", ui::collaboration::SpeakerRoleSpec::Left),
                            ("Shelf", ui::collaboration::SpeakerRoleSpec::Right),
                        ]
                        .into_iter()
                        .map(|(device, role)| ui::collaboration::SpeakerMemberInfo {
                            device: device.into(),
                            device_label: device.into(),
                            output: "0a".repeat(16),
                            output_label: None,
                            role,
                            status: ui::collaboration::SpeakerMemberStatus::Ready,
                            trim: ui::collaboration::SpeakerTrimInfo::default(),
                        })
                        .collect(),
                        playback: None,
                        microphones: Vec::new(),
                        output_here: false,
                    }],
                    ..SpeakerSnapshot::default()
                },
            },
            _ => SpeakerReply::Done { group: None },
        })
    }
}

pub(super) struct NoSpeakerSets;

impl crate::speaker_setup::SpeakerService for NoSpeakerSets {
    fn call(
        &self,
        _: ui::collaboration::SpeakerCommand,
    ) -> Result<ui::collaboration::SpeakerReply, String> {
        Ok(ui::collaboration::SpeakerReply::Snapshot {
            snapshot: ui::collaboration::SpeakerSnapshot::default(),
        })
    }
}
