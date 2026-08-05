//! HackRF half-duplex RX/TX driver.

mod common;

#[cfg(any(target_arch = "wasm32", feature = "smol", feature = "tokio"))]
mod asynchronous;
#[cfg(any(target_arch = "wasm32", feature = "smol", feature = "tokio"))]
pub use asynchronous::{AsyncHackRf, AsyncHackRfRxStreamer, AsyncHackRfTxStreamer};

#[cfg(not(target_arch = "wasm32"))]
mod sync;
#[cfg(not(target_arch = "wasm32"))]
pub use sync::{HackRf, RxStreamer, TxStreamer};
