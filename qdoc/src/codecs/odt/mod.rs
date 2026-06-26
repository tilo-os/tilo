//! `.odt` (OpenDocument Text) codec. Lossless-preserving: the ODF container's
//! parts are carried verbatim except the `content.xml` body, which maps onto the
//! canonical model. Built across Phase 4: container (here) → body parse → spans
//! → export → preservation.

pub mod container;
