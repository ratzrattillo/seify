use serde::{Deserialize, Serialize};

use crate::device::DynDeviceBackend;
use crate::{Direction, Error, Range};

/// Structured runtime capabilities for a device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    /// Whether this device supports simultaneous reception and transmission.
    pub full_duplex: bool,
    /// RX channels exposed by this device.
    pub rx_channels: Vec<ChannelCapabilities>,
    /// TX channels exposed by this device.
    pub tx_channels: Vec<ChannelCapabilities>,
}

impl DeviceCapabilities {
    /// Build a capability snapshot from a runtime-dispatched backend.
    pub fn from_dyn<D: DynDeviceBackend + ?Sized>(dev: &D) -> Result<Self, Error> {
        Ok(Self {
            full_duplex: dev.full_duplex()?,
            rx_channels: channel_capabilities(dev, Direction::Rx)?,
            tx_channels: channel_capabilities(dev, Direction::Tx)?,
        })
    }
}

/// Structured runtime capabilities for one channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelCapabilities {
    /// Channel index within its direction.
    pub channel: usize,
    /// Optional controls exposed by this channel.
    pub controls: ChannelControls,
}

/// Optional controls exposed by one channel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelControls {
    /// Available antenna ports.
    pub antennas: Option<Vec<String>>,
    /// Automatic gain control support.
    pub agc: bool,
    /// Named gain elements.
    pub gain_elements: Option<Vec<String>>,
    /// Overall gain range.
    pub gain_range: Option<Range>,
    /// Named frequency components.
    pub frequency_components: Option<Vec<String>>,
    /// Overall frequency range.
    pub frequency_range: Option<Range>,
    /// Baseband sample-rate range.
    pub sample_rate_range: Option<Range>,
    /// Hardware bandwidth range.
    pub bandwidth_range: Option<Range>,
    /// Automatic DC offset correction support.
    pub dc_offset: bool,
}

fn channel_capabilities<D>(dev: &D, direction: Direction) -> Result<Vec<ChannelCapabilities>, Error>
where
    D: DynDeviceBackend + ?Sized,
{
    let channels = dev.num_channels(direction)?;

    (0..channels)
        .map(|channel| {
            Ok(ChannelCapabilities {
                channel,
                controls: ChannelControls {
                    antennas: optional_dyn_capability(dev.antenna_control(), |cap| {
                        cap.antennas(direction, channel)
                    })?,
                    agc: dyn_capability_available(dev.agc_control(), |cap| {
                        cap.agc_available(direction, channel)
                    })?,
                    gain_elements: optional_dyn_capability(dev.gain_control(), |cap| {
                        cap.gain_elements(direction, channel)
                    })?,
                    gain_range: optional_dyn_capability(dev.gain_control(), |cap| {
                        cap.gain_range(direction, channel)
                    })?,
                    frequency_components: optional_dyn_capability(
                        dev.frequency_control(),
                        |cap| cap.frequency_components(direction, channel),
                    )?,
                    frequency_range: optional_dyn_capability(dev.frequency_control(), |cap| {
                        cap.frequency_range(direction, channel)
                    })?,
                    sample_rate_range: optional_dyn_capability(dev.sample_rate_control(), |cap| {
                        cap.get_sample_rate_range(direction, channel)
                    })?,
                    bandwidth_range: optional_dyn_capability(dev.bandwidth_control(), |cap| {
                        cap.get_bandwidth_range(direction, channel)
                    })?,
                    dc_offset: dyn_capability_available(dev.dc_offset_control(), |cap| {
                        cap.dc_offset_available(direction, channel)
                    })?,
                },
            })
        })
        .collect()
}

fn optional_capability<T>(result: Result<T, Error>) -> Result<Option<T>, Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.is_unsupported() => Ok(None),
        Err(e) => Err(e),
    }
}

fn optional_dyn_capability<C: ?Sized, T>(
    cap: Option<&C>,
    f: impl FnOnce(&C) -> Result<T, Error>,
) -> Result<Option<T>, Error> {
    match cap {
        Some(cap) => optional_capability(f(cap)),
        None => Ok(None),
    }
}

fn dyn_capability_available<C: ?Sized>(
    cap: Option<&C>,
    f: impl FnOnce(&C) -> Result<bool, Error>,
) -> Result<bool, Error> {
    match cap {
        Some(cap) => match f(cap) {
            Ok(available) => Ok(available),
            Err(e) if e.is_unsupported() => Ok(false),
            Err(e) => Err(e),
        },
        None => Ok(false),
    }
}
