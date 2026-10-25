mod reservation;
pub use reservation::{
    NativeReplacementPurpose, NativeReservationDescription, NativeReservationFacts,
    NativeReservationLocator,
};

pub const LIMIT: usize = 4096;
