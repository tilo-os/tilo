//! A sound card in memory, for the tests that host the audio service: the
//! service reads and controls the graph the test hands it, and the actions a
//! test invokes reach that graph.

use std::sync::Mutex;

use media::audio::{AudioGraph, GraphControl};
use tilo_audio_service::sound::{Graph, Read};

/// A sound card in memory: the graph the service reads, in use.
pub struct Card {
    graph: Mutex<AudioGraph>,
}

impl Card {
    pub fn new(graph: AudioGraph) -> Self {
        Self {
            graph: Mutex::new(graph),
        }
    }
}

impl Graph for Card {
    fn read(&self) -> Read {
        Read {
            graph: self.graph.lock().unwrap().clone(),
            uses: Vec::new(),
        }
    }

    fn control(&self) -> &dyn GraphControl {
        self
    }
}

impl GraphControl for Card {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
        for sink in &mut self.graph.lock().unwrap().sinks {
            sink.is_default = sink.id == node_id;
        }
        Ok(())
    }

    fn set_default_source(&self, _node_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn route_stream(&self, _stream_id: u32, _target_serial: u32) -> Result<(), String> {
        Ok(())
    }

    fn unroute_stream(&self, _stream_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        for sink in &mut self.graph.lock().unwrap().sinks {
            if sink.id == node_id {
                sink.volume = Some(perceptual);
            }
        }
        Ok(())
    }

    fn set_node_mute(&self, _node_id: u32, _muted: bool) -> Result<(), String> {
        Ok(())
    }

    fn set_device_profile(&self, _device_id: u32, _profile_index: u32) -> Result<(), String> {
        Ok(())
    }
}
