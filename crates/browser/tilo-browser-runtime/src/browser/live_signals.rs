//! What this person signals to the room on the page's live presence beside
//! where they are: their latest wave, and whose view they follow. Each goes
//! into the status the page reads only when it changed; the page restates
//! it with every presence frame while it lasts.

use super::Mount;

impl Mount {
    /// Add `liveWave` and `liveFollowing` to the status `value` where they
    /// changed since the page was last told.
    pub(super) fn add_live_signals(&self, value: &mut serde_json::Value) {
        // This person's latest wave, for the page's presence to carry.
        let wave = self.desktop.as_ref().and_then(|desktop| {
            desktop
                .here
                .borrow()
                .sent_wave()
                .and_then(|wave| serde_json::to_value(wave).ok())
        });
        if *self.live_wave.borrow() != wave
            && let Some(object) = value.as_object_mut()
        {
            object.insert(
                "liveWave".into(),
                wave.clone().unwrap_or(serde_json::Value::Null),
            );
            *self.live_wave.borrow_mut() = wave;
        }
        // Whom this person follows, for the page's presence to say while
        // they do: everyone's roster shows it, as a native host's does.
        let following = self
            .desktop
            .as_ref()
            .and_then(|desktop| desktop.here.borrow().following().map(str::to_owned));
        if *self.live_following.borrow() != following
            && let Some(object) = value.as_object_mut()
        {
            object.insert("liveFollowing".into(), following.clone().into());
            *self.live_following.borrow_mut() = following;
        }
    }
}
