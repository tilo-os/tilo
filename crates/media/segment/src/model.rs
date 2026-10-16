//! The subject of a picture found by a local salient-object model.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "pixel indices are bounded by the picture's size, checked once at the start; the rest is float maths on colours and coverages"
)]

use rten::{Model, RunOptions, ThreadPool};
use rten_tensor::prelude::*;
use rten_tensor::{NdTensor, NdTensorView};
use std::sync::Arc;

use crate::{Error, Matte, Picture};

/// How a model wants its picture: the side it sees, and how colours are
/// scaled before it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// U²-Net and its small sibling U²-Netp: 320 pixels a side, colours
    /// scaled by the picture's brightest then standardised as `ImageNet`'s.
    U2Net,
    /// IS-Net (DIS): 1024 pixels a side, colours scaled by the brightest
    /// then centred on a half.
    IsNet,
}

impl Family {
    /// The side of the square the model sees, in pixels.
    pub const fn side(self) -> usize {
        match self {
            Self::U2Net => 320,
            Self::IsNet => 1024,
        }
    }

    /// Mean and spread each channel is standardised by.
    const fn standard(self) -> ([f32; 3], [f32; 3]) {
        match self {
            Self::U2Net => ([0.485, 0.456, 0.406], [0.229, 0.224, 0.225]),
            Self::IsNet => ([0.5, 0.5, 0.5], [1.0, 1.0, 1.0]),
        }
    }
}

/// A loaded salient-object model.
pub struct SubjectModel {
    model: Model,
    family: Family,
    threads: Option<Arc<ThreadPool>>,
}

impl SubjectModel {
    /// Load an ONNX model of `family` from its bytes.
    ///
    /// # Errors
    /// Returns why the bytes are not a model of one input and output.
    pub fn load(bytes: Vec<u8>, family: Family) -> Result<Self, Error> {
        let model = Model::load(bytes).map_err(|error| Error::Load(error.to_string()))?;
        if model.input_ids().is_empty() || model.output_ids().is_empty() {
            return Err(Error::Load("it has no input or no output".into()));
        }
        Ok(Self {
            model,
            family,
            threads: None,
        })
    }

    /// Run on `threads` threads rather than one per core.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.threads = Some(Arc::new(ThreadPool::with_num_threads(threads.max(1))));
        self
    }

    pub const fn family(&self) -> Family {
        self.family
    }

    /// How much of each pixel of `picture` is its subject, at the
    /// picture's own size.
    ///
    /// # Errors
    /// Returns why the picture cannot be read, or the model's failure.
    pub fn matte(&self, picture: &Picture) -> Result<Matte, Error> {
        picture.check()?;
        let side = self.family.side();
        let input = self.input(picture, side);
        let mut options = RunOptions::default();
        options.thread_pool.clone_from(&self.threads);
        let output = self
            .model
            .run_one(input.view().into(), Some(options))
            .map_err(|error| Error::Run(error.to_string()))?;
        let output: NdTensor<f32, 4> = output
            .try_into()
            .map_err(|_| Error::Run("its answer is not one coverage map".into()))?;
        let view: NdTensorView<f32, 4> = output.view();
        let [_, _, rows, columns] = view.shape();
        if rows == 0 || columns == 0 {
            return Err(Error::Run("its answer is empty".into()));
        }
        let values: Vec<f32> = view.iter().take(rows * columns).copied().collect();
        // Spread to the full range, as the model's own reference code does:
        // its raw answer rarely reaches 0 or 1.
        let (low, high) = values
            .iter()
            .fold((f32::MAX, f32::MIN), |(low, high), &value| {
                (low.min(value), high.max(value))
            });
        let spread = (high - low).max(1e-6);
        let values = values.iter().map(|value| (value - low) / spread).collect();
        let matte = Matte::new(columns, rows, values)
            .ok_or_else(|| Error::Run("its answer has the wrong size".into()))?;
        Ok(matte.resized(picture.width, picture.height))
    }

    /// The picture as the model's input: `side` square, channels first.
    fn input(&self, picture: &Picture, side: usize) -> NdTensor<f32, 4> {
        let (mean, spread) = self.family.standard();
        let mut planes = Vec::with_capacity(3 * side * side);
        let mut brightest = 0.0_f32;
        let channels: Vec<Matte> = (0..3)
            .map(|c| {
                let values: Vec<f32> = picture
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| f32::from(pixel.get(c).copied().unwrap_or(0)) / 255.0)
                    .collect();
                Matte::new(picture.width, picture.height, values).map_or_else(
                    || Matte {
                        width: side,
                        height: side,
                        values: vec![0.0; side * side],
                    },
                    |plane| plane.resized(side, side),
                )
            })
            .collect();
        for plane in &channels {
            brightest = plane.values.iter().copied().fold(brightest, f32::max);
        }
        let brightest = brightest.max(1e-6);
        for (c, plane) in channels.iter().enumerate() {
            planes.extend(
                plane
                    .values
                    .iter()
                    .map(|value| (value / brightest - mean[c]) / spread[c]),
            );
        }
        NdTensor::from_data([1, 3, side, side], planes)
    }
}
