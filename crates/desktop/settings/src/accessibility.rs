//! The `accessibility` section's screen-reader keys, which every toolkit app
//! reads to run its narrator. The whole section, with the pointer, typing,
//! hearing and vision assists Settings edits and the compositor applies, is
//! the `accessibility-settings` crate's model; it reads these keys through
//! the definitions here, so the two cannot disagree about a default or a range.

pub use settings_model::accessibility::{
    DEFAULT_SCREEN_READER_RATE, DEVICE_SECTION, SCREEN_READER_RATE_RANGE, SECTION,
    ScreenReaderSettings, ScreenReaderVoice, clamped_rate,
};

pub const SCREEN_READER: crate::Key<bool> = crate::Key::new(
    SECTION,
    settings_model::accessibility::SCREEN_READER_KEY,
    || false,
);

pub trait Labelled: Copy + PartialEq + 'static {
    const CHOICES: &'static [Self];

    fn label(self) -> &'static str;
}

#[doc(hidden)]
#[macro_export]
macro_rules! labelled_choice {
    ($(#[$meta:meta])* $name:ident { $($(#[$variant_meta:meta])* $variant:ident => $label:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[cfg_attr(feature = "model", derive(tilo_resource::Model))]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($(#[$variant_meta])* $variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const OPTIONS: &'static [&'static str] = &[$($label),+];

            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label),+
                }
            }

            pub fn from_label(label: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|choice| choice.label() == label)
            }
        }

        impl $crate::Labelled for $name {
            const CHOICES: &'static [Self] = Self::ALL;

            fn label(self) -> &'static str {
                Self::label(self)
            }
        }
    };
}

impl Labelled for ScreenReaderVoice {
    const CHOICES: &'static [Self] = Self::ALL;

    fn label(self) -> &'static str {
        Self::label(self)
    }
}
