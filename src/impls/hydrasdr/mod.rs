//! HydraSDR RFOne driver.

mod common;

#[cfg(any(feature = "smol", feature = "tokio"))]
mod asynchronous;
#[cfg(any(feature = "smol", feature = "tokio"))]
pub use asynchronous::{AsyncHydraSdr, AsyncHydraSdrRxStreamer};

#[cfg(not(target_arch = "wasm32"))]
mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub use sync::{HydraSdr, RxStreamer, TxDummy};
