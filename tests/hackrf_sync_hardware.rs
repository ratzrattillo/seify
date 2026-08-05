#![cfg(all(feature = "hackrf", not(target_arch = "wasm32")))]

use num_complex::Complex32;
use seify::{Error, Registry, RxStreamer, TxStreamer};

const READ_TIMEOUT_US: i64 = 500_000;

#[test]
#[ignore = "requires an attached HackRF"]
fn sync_hackrf_half_duplex() -> Result<(), Error> {
    let registry = Registry::default();
    let descriptors = registry.probe("driver=hackrf")?;
    let descriptor = descriptors.first().ok_or(Error::DeviceNotFound)?;
    let device = registry.open(descriptor)?;
    let rx = device.rx(0)?;
    let tx = device.tx(0)?;

    let mut rx_stream = rx.streamer()?;
    let mut tx_stream = tx.streamer()?;
    let mut received = [Complex32::default(); 4096];
    let zeros = [Complex32::default(); 4096];

    rx_stream.activate()?;
    assert!(rx_stream.read(&mut [&mut received], READ_TIMEOUT_US)? > 0);

    tx_stream.activate()?;
    assert_eq!(
        tx_stream.write(&[&zeros], None, true, READ_TIMEOUT_US)?,
        zeros.len()
    );

    // RX was not deactivated. Its next read retakes the half-duplex radio.
    assert!(rx_stream.read(&mut [&mut received], READ_TIMEOUT_US)? > 0);

    tx_stream.deactivate()?;
    rx_stream.deactivate()?;
    Ok(())
}
