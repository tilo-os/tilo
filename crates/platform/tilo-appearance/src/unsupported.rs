//! A target with no appearance source of its own.

use crate::Appearance;

#[derive(Default)]
pub(crate) struct Reading;

pub(crate) fn follow(_changed: impl Fn() + Send + 'static) -> Reading {
    Reading
}

pub(crate) fn current(_reading: &Reading) -> Appearance {
    Appearance::default()
}
