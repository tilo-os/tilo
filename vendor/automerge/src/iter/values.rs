use crate::{exid::ExId, types, types::Clock};

use crate::op_set2::op_set::OpQueryTerm;
use crate::op_set2::OpSet;

use std::fmt::Debug;

#[cfg(not(tilo_model_guest))]
#[derive(Default, Debug)]
pub struct Values<'a> {
    iter: Option<(&'a OpSet, Box<dyn OpQueryTerm<'a> + 'a>)>,
}

/// TILO(model-device): the values the host of a model module's document read.
#[cfg(tilo_model_guest)]
#[derive(Default, Debug)]
pub struct Values<'a> {
    pub(crate) hosted: crate::model_device::Hosted<'a, (types::Value<'static>, ExId)>,
}

impl<'a> Values<'a> {
    #[cfg(tilo_model_guest)]
    pub(crate) fn new<I: OpQueryTerm<'a> + 'a>(_: &'a OpSet, _: I, _: Option<Clock>) -> Self {
        crate::model_device::outside_device("an op set")
    }

    // FIXME - ignore clock?
    #[cfg(not(tilo_model_guest))]
    pub(crate) fn new<I: OpQueryTerm<'a> + 'a>(
        op_set: &'a OpSet,
        iter: I,
        _clock: Option<Clock>,
    ) -> Self {
        Self {
            iter: Some((op_set, Box::new(iter))),
        }
    }
}

impl<'a> Iterator for Values<'a> {
    type Item = (types::Value<'a>, ExId);

    #[cfg(tilo_model_guest)]
    fn next(&mut self) -> Option<Self::Item> {
        self.hosted.next()
    }

    #[cfg(not(tilo_model_guest))]
    fn next(&mut self) -> Option<Self::Item> {
        let (op_set, iter) = self.iter.as_mut()?;
        let op = iter.next()?;
        let value = op.value().to_value();
        let id = op_set.id_to_exid(op.id);
        Some((value, id))
    }
}
