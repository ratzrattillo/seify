#![allow(dead_code)]
#![allow(unused_variables)]
use std::any::Any;
use std::sync::Arc;

use crate::registry::TypedDeviceBackend;
use crate::Args;
use crate::Capability;
use crate::DeviceCapabilities;
use crate::Direction;
use crate::Driver;
use crate::Error;
use crate::Range;
use crate::Registry;
use crate::RxStreamer;
use crate::TxStreamer;

/// Type-erased RX streamer.
pub type DynRxStreamer = Box<dyn RxStreamer>;

/// Type-erased TX streamer.
pub type DynTxStreamer = Box<dyn TxStreamer>;

/// Object-safe RX streaming capability for runtime-dispatched devices.
pub trait DynRxDevice: DeviceInfo {
    /// Create a type-erased RX streamer.
    fn rx_streamer(&self, channels: &[usize], args: Args) -> Result<DynRxStreamer, Error>;
}

impl<T> DynRxDevice for T
where
    T: RxDevice,
    T::RxStreamer: 'static,
{
    fn rx_streamer(&self, channels: &[usize], args: Args) -> Result<DynRxStreamer, Error> {
        Ok(Box::new(RxDevice::rx_streamer(self, channels, args)?))
    }
}

/// Object-safe TX streaming capability for runtime-dispatched devices.
pub trait DynTxDevice: DeviceInfo {
    /// Create a type-erased TX streamer.
    fn tx_streamer(&self, channels: &[usize], args: Args) -> Result<DynTxStreamer, Error>;
}

impl<T> DynTxDevice for T
where
    T: TxDevice,
    T::TxStreamer: 'static,
{
    fn tx_streamer(&self, channels: &[usize], args: Args) -> Result<DynTxStreamer, Error> {
        Ok(Box::new(TxDevice::tx_streamer(self, channels, args)?))
    }
}

/// Runtime-dispatched device backend.
///
/// The dynamic backend exposes fundamental device metadata and optional views
/// into streaming and control capability traits.
pub trait DynDeviceBackend: DeviceInfo + Send + Sync + Any {
    /// Return RX streaming capability, if the backend exposes it.
    fn rx_device(&self) -> Option<&dyn DynRxDevice> {
        None
    }

    /// Return TX streaming capability, if the backend exposes it.
    fn tx_device(&self) -> Option<&dyn DynTxDevice> {
        None
    }

    /// Return antenna control capability, if the backend exposes it.
    fn antenna_control(&self) -> Option<&dyn AntennaControl> {
        None
    }

    /// Return automatic gain control capability, if the backend exposes it.
    fn agc_control(&self) -> Option<&dyn AgcControl> {
        None
    }

    /// Return gain control capability, if the backend exposes it.
    fn gain_control(&self) -> Option<&dyn GainControl> {
        None
    }

    /// Return frequency control capability, if the backend exposes it.
    fn frequency_control(&self) -> Option<&dyn FrequencyControl> {
        None
    }

    /// Return sample-rate control capability, if the backend exposes it.
    fn sample_rate_control(&self) -> Option<&dyn SampleRateControl> {
        None
    }

    /// Return bandwidth control capability, if the backend exposes it.
    fn bandwidth_control(&self) -> Option<&dyn BandwidthControl> {
        None
    }

    /// Return DC offset control capability, if the backend exposes it.
    fn dc_offset_control(&self) -> Option<&dyn DcOffsetControl> {
        None
    }
}

/// Runtime-dispatched opened device.
///
/// This is used to control a device when the concrete driver implementation is
/// not known at compile time.
#[derive(Clone)]
pub struct DynDevice {
    inner: Arc<dyn DynDeviceBackend>,
}

/// Basic device metadata.
pub trait DeviceInfo: Send + Sync {
    /// SDR [driver](Driver).
    fn driver(&self) -> Driver;
    /// Identifier for the device, e.g. its serial.
    fn id(&self) -> Result<String, Error>;
    /// Device info that can be displayed to the user.
    fn info(&self) -> Result<Args, Error>;
    /// Number of supported channels.
    fn num_channels(&self, direction: Direction) -> Result<usize, Error>;
    /// Whether this device supports simultaneous reception and transmission.
    fn full_duplex(&self) -> Result<bool, Error>;
}

/// RX streaming capability.
pub trait RxDevice: DeviceInfo {
    /// RX streamer implementation.
    type RxStreamer: RxStreamer;

    /// Create an RX streamer.
    fn rx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::RxStreamer, Error>;
}

/// TX streaming capability.
pub trait TxDevice: DeviceInfo {
    /// TX streamer implementation.
    type TxStreamer: TxStreamer;

    /// Create a TX streamer.
    fn tx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::TxStreamer, Error>;
}

/// Antenna control capability.
pub trait AntennaControl: DeviceInfo {
    /// Return available antenna port names.
    fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error>;
    /// Return the currently selected antenna port name.
    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error>;
    /// Select an antenna port by name.
    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error>;
}

/// Automatic gain control capability.
pub trait AgcControl: DeviceInfo {
    /// Return whether automatic gain control is available.
    fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error>;
    /// Return whether automatic gain control is currently enabled.
    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error>;
    /// Enable or disable automatic gain control.
    fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error>;
}

/// Gain control capability.
pub trait GainControl: DeviceInfo {
    /// Return named gain elements available for the channel.
    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error>;
    /// Set overall channel gain in dB.
    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error>;
    /// Return overall channel gain in dB, if available.
    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error>;
    /// Return supported overall channel gain range in dB.
    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error>;
    /// Set a named gain element in dB.
    fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error>;
    /// Return a named gain element in dB, if available.
    fn gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error>;
    /// Return supported range in dB for a named gain element.
    fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error>;
}

/// Frequency control capability.
pub trait FrequencyControl: DeviceInfo {
    /// Return supported overall tuning range in Hz.
    fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error>;
    /// Return current overall channel frequency in Hz.
    fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error>;
    /// Set overall channel frequency in Hz with optional driver arguments.
    fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        args: Args,
    ) -> Result<(), Error>;
    /// Return named frequency components for the channel.
    fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error>;
    /// Return supported range in Hz for a named frequency component.
    fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error>;
    /// Return current frequency in Hz for a named frequency component.
    fn component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error>;
    /// Set frequency in Hz for a named frequency component.
    fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error>;
}

/// Sample-rate control capability.
pub trait SampleRateControl: DeviceInfo {
    /// Return current sample rate in samples per second.
    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error>;
    /// Set sample rate in samples per second.
    fn set_sample_rate(&self, direction: Direction, channel: usize, rate: f64)
        -> Result<(), Error>;
    /// Return supported sample-rate range in samples per second.
    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error>;
}

/// Bandwidth control capability.
pub trait BandwidthControl: DeviceInfo {
    /// Return current channel bandwidth in Hz.
    fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error>;
    /// Set channel bandwidth in Hz.
    fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<(), Error>;
    /// Return supported channel bandwidth range in Hz.
    fn get_bandwidth_range(&self, direction: Direction, channel: usize) -> Result<Range, Error>;
}

/// Automatic DC offset correction capability.
pub trait DcOffsetControl: DeviceInfo {
    /// Return whether automatic DC offset correction is available.
    fn dc_offset_available(&self, direction: Direction, channel: usize) -> Result<bool, Error>;
    /// Return whether automatic DC offset correction is enabled.
    fn dc_offset_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error>;
    /// Enable or disable automatic DC offset correction.
    fn set_dc_offset_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error>;
}

/// RX channel handle.
pub struct RxChannel<'a, T: ?Sized> {
    dev: &'a T,
    channel: usize,
}

impl<'a, T: ?Sized> RxChannel<'a, T> {
    fn new(dev: &'a T, channel: usize) -> Self {
        Self { dev, channel }
    }

    /// Channel index.
    pub fn id(&self) -> usize {
        self.channel
    }

    /// Channel index.
    pub fn index(&self) -> usize {
        self.channel
    }
}

/// TX channel handle.
pub struct TxChannel<'a, T: ?Sized> {
    dev: &'a T,
    channel: usize,
}

impl<'a, T: ?Sized> TxChannel<'a, T> {
    fn new(dev: &'a T, channel: usize) -> Self {
        Self { dev, channel }
    }

    /// Channel index.
    pub fn id(&self) -> usize {
        self.channel
    }

    /// Channel index.
    pub fn index(&self) -> usize {
        self.channel
    }
}

/// Antenna control handle for one channel.
pub struct Antenna<'a, T: AntennaControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> Antenna<'a, T>
where
    T: AntennaControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    /// Selectable antenna ports.
    pub fn ports(&self) -> Result<Vec<String>, Error> {
        self.dev.antennas(self.direction, self.channel)
    }

    /// Currently selected antenna.
    pub fn selected(&self) -> Result<String, Error> {
        self.dev.antenna(self.direction, self.channel)
    }

    /// Select an antenna port.
    pub fn select(&self, name: &str) -> Result<(), Error> {
        self.dev.set_antenna(self.direction, self.channel, name)
    }
}

/// Automatic gain control handle for one channel.
pub struct Agc<'a, T: AgcControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> Agc<'a, T>
where
    T: AgcControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    fn ensure_available(&self) -> Result<(), Error> {
        if self.dev.agc_available(self.direction, self.channel)? {
            Ok(())
        } else {
            Err(Error::unsupported(Capability::Agc))
        }
    }

    /// Return whether automatic gain control is enabled.
    pub fn enabled(&self) -> Result<bool, Error> {
        self.ensure_available()?;
        self.dev.agc_enabled(self.direction, self.channel)
    }

    /// Enable automatic gain control.
    pub fn enable(&self) -> Result<(), Error> {
        self.set_enabled(true)
    }

    /// Disable automatic gain control.
    pub fn disable(&self) -> Result<(), Error> {
        self.set_enabled(false)
    }

    /// Set whether automatic gain control is enabled.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), Error> {
        self.ensure_available()?;
        self.dev
            .set_agc_enabled(self.direction, self.channel, enabled)
    }
}

/// Gain control handle for one channel.
pub struct Gain<'a, T: GainControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> Gain<'a, T>
where
    T: GainControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    /// Named gain elements.
    pub fn elements(&self) -> Result<Vec<String>, Error> {
        self.dev.gain_elements(self.direction, self.channel)
    }

    /// Overall gain.
    pub fn value(&self) -> Result<Option<f64>, Error> {
        self.dev.gain(self.direction, self.channel)
    }

    /// Set overall gain.
    pub fn set(&self, gain: f64) -> Result<(), Error> {
        self.dev.set_gain(self.direction, self.channel, gain)
    }

    /// Overall gain range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev.gain_range(self.direction, self.channel)
    }

    /// Named gain element handle.
    pub fn element(&self, name: &str) -> GainElement<'a, T> {
        GainElement {
            dev: self.dev,
            direction: self.direction,
            channel: self.channel,
            name: name.to_string(),
        }
    }
}

/// Gain element handle for one channel.
pub struct GainElement<'a, T: GainControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
    name: String,
}

impl<'a, T> GainElement<'a, T>
where
    T: GainControl + ?Sized,
{
    /// Gain element value.
    pub fn value(&self) -> Result<Option<f64>, Error> {
        self.dev
            .gain_element(self.direction, self.channel, &self.name)
    }

    /// Set gain element value.
    pub fn set(&self, gain: f64) -> Result<(), Error> {
        self.dev
            .set_gain_element(self.direction, self.channel, &self.name, gain)
    }

    /// Gain element range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev
            .gain_element_range(self.direction, self.channel, &self.name)
    }
}

/// Frequency control handle for one channel.
pub struct Frequency<'a, T: FrequencyControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> Frequency<'a, T>
where
    T: FrequencyControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    /// Overall frequency.
    pub fn value(&self) -> Result<f64, Error> {
        self.dev.frequency(self.direction, self.channel)
    }

    /// Set overall frequency.
    pub fn set(&self, frequency: f64) -> Result<(), Error> {
        self.set_with_args(frequency, Args::new())
    }

    /// Set overall frequency with driver arguments.
    pub fn set_with_args(&self, frequency: f64, args: Args) -> Result<(), Error> {
        self.dev
            .set_frequency(self.direction, self.channel, frequency, args)
    }

    /// Overall frequency range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev.frequency_range(self.direction, self.channel)
    }

    /// Named frequency components.
    pub fn components(&self) -> Result<Vec<String>, Error> {
        self.dev.frequency_components(self.direction, self.channel)
    }

    /// Named frequency component handle.
    pub fn component(&self, name: &str) -> FrequencyComponent<'a, T> {
        FrequencyComponent {
            dev: self.dev,
            direction: self.direction,
            channel: self.channel,
            name: name.to_string(),
        }
    }
}

/// Frequency component handle for one channel.
pub struct FrequencyComponent<'a, T: FrequencyControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
    name: String,
}

impl<'a, T> FrequencyComponent<'a, T>
where
    T: FrequencyControl + ?Sized,
{
    /// Frequency component value.
    pub fn value(&self) -> Result<f64, Error> {
        self.dev
            .component_frequency(self.direction, self.channel, &self.name)
    }

    /// Set frequency component value.
    pub fn set(&self, frequency: f64) -> Result<(), Error> {
        self.dev
            .set_component_frequency(self.direction, self.channel, &self.name, frequency)
    }

    /// Frequency component range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev
            .component_frequency_range(self.direction, self.channel, &self.name)
    }
}

/// Sample-rate control handle for one channel.
pub struct SampleRate<'a, T: SampleRateControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> SampleRate<'a, T>
where
    T: SampleRateControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    /// Sample rate.
    pub fn value(&self) -> Result<f64, Error> {
        self.dev.sample_rate(self.direction, self.channel)
    }

    /// Set sample rate.
    pub fn set(&self, rate: f64) -> Result<(), Error> {
        self.dev.set_sample_rate(self.direction, self.channel, rate)
    }

    /// Sample-rate range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev.get_sample_rate_range(self.direction, self.channel)
    }
}

/// Bandwidth control handle for one channel.
pub struct Bandwidth<'a, T: BandwidthControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> Bandwidth<'a, T>
where
    T: BandwidthControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    /// Bandwidth.
    pub fn value(&self) -> Result<f64, Error> {
        self.dev.bandwidth(self.direction, self.channel)
    }

    /// Set bandwidth.
    pub fn set(&self, bandwidth: f64) -> Result<(), Error> {
        self.dev
            .set_bandwidth(self.direction, self.channel, bandwidth)
    }

    /// Bandwidth range.
    pub fn range(&self) -> Result<Range, Error> {
        self.dev.get_bandwidth_range(self.direction, self.channel)
    }
}

/// Automatic DC offset correction handle for one channel.
pub struct DcOffset<'a, T: DcOffsetControl + ?Sized> {
    dev: &'a T,
    direction: Direction,
    channel: usize,
}

impl<'a, T> DcOffset<'a, T>
where
    T: DcOffsetControl + ?Sized,
{
    fn new(dev: &'a T, direction: Direction, channel: usize) -> Self {
        Self {
            dev,
            direction,
            channel,
        }
    }

    fn ensure_available(&self) -> Result<(), Error> {
        if self.dev.dc_offset_available(self.direction, self.channel)? {
            Ok(())
        } else {
            Err(Error::unsupported(Capability::DcOffset))
        }
    }

    /// Return whether automatic DC offset correction is enabled.
    pub fn enabled(&self) -> Result<bool, Error> {
        self.ensure_available()?;
        self.dev.dc_offset_enabled(self.direction, self.channel)
    }

    /// Enable automatic DC offset correction.
    pub fn enable(&self) -> Result<(), Error> {
        self.set_enabled(true)
    }

    /// Disable automatic DC offset correction.
    pub fn disable(&self) -> Result<(), Error> {
        self.set_enabled(false)
    }

    /// Set whether automatic DC offset correction is enabled.
    pub fn set_enabled(&self, enabled: bool) -> Result<(), Error> {
        self.ensure_available()?;
        self.dev
            .set_dc_offset_enabled(self.direction, self.channel, enabled)
    }
}

/// Wraps a driver implementation.
///
/// Implements a more ergonomic version of the backend APIs, e.g. using
/// `Into<Args>`, which would not be possible in traits.
#[derive(Clone)]
pub struct Device<T> {
    dev: T,
}

impl<T> Device<T>
where
    T: DeviceInfo + Clone,
{
    /// Create a device from the device implementation.
    pub fn from_impl(dev: T) -> Self {
        Self { dev }
    }

    /// Borrow the underlying device implementation.
    pub fn as_inner(&self) -> &T {
        &self.dev
    }

    /// Mutably borrow the underlying device implementation.
    pub fn as_inner_mut(&mut self) -> &mut T {
        &mut self.dev
    }

    /// Consume this device and return the underlying implementation.
    pub fn into_inner(self) -> T {
        self.dev
    }
}

impl<T> Device<T>
where
    T: TypedDeviceBackend,
{
    /// Open a typed device matching `args`.
    pub fn from_args<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        match args.get::<Driver>("driver") {
            Ok(driver) if driver != <T as TypedDeviceBackend>::driver() => {
                return Err(Error::DriverMismatch {
                    expected: <T as TypedDeviceBackend>::driver(),
                    requested: driver,
                });
            }
            Ok(_) | Err(Error::MissingArgument { .. }) => {}
            Err(e) => return Err(e),
        }
        Ok(Self::from_impl(T::open(&args)?))
    }
}

impl<T> Device<T>
where
    T: DynDeviceBackend + Clone + 'static,
{
    /// Clone this typed device into a runtime-dispatched device.
    ///
    /// Backend clones must refer to the same opened logical device and share
    /// configuration state.
    pub fn to_dyn(&self) -> DynDevice {
        DynDevice::from_impl(self.dev.clone())
    }

    /// Convert this typed device into a runtime-dispatched device.
    pub fn into_dyn(self) -> DynDevice {
        DynDevice::from_impl(self.dev)
    }
}

impl<T> Device<T>
where
    T: DynDeviceBackend,
{
    /// Structured runtime capabilities for the device.
    pub fn capabilities(&self) -> Result<DeviceCapabilities, Error> {
        DeviceCapabilities::from_dyn(&self.dev)
    }
}

impl<T: DeviceInfo> Device<T> {
    /// SDR [driver](Driver).
    pub fn driver(&self) -> Driver {
        self.dev.driver()
    }

    /// Identifier for the device, e.g. its serial.
    pub fn id(&self) -> Result<String, Error> {
        self.dev.id()
    }

    /// Device info that can be displayed to the user.
    pub fn info(&self) -> Result<Args, Error> {
        self.dev.info()
    }

    /// Number of supported channels.
    pub fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        self.dev.num_channels(direction)
    }

    /// Whether this device supports simultaneous reception and transmission.
    pub fn full_duplex(&self) -> Result<bool, Error> {
        self.dev.full_duplex()
    }
}

impl<T: DeviceInfo> DeviceInfo for Device<T> {
    fn driver(&self) -> Driver {
        self.dev.driver()
    }

    fn id(&self) -> Result<String, Error> {
        self.dev.id()
    }

    fn info(&self) -> Result<Args, Error> {
        self.dev.info()
    }

    fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        self.dev.num_channels(direction)
    }

    fn full_duplex(&self) -> Result<bool, Error> {
        self.dev.full_duplex()
    }
}

impl<T: RxDevice> RxDevice for Device<T> {
    type RxStreamer = T::RxStreamer;

    fn rx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::RxStreamer, Error> {
        self.dev.rx_streamer(channels, args)
    }
}

impl<T: TxDevice> TxDevice for Device<T> {
    type TxStreamer = T::TxStreamer;

    fn tx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::TxStreamer, Error> {
        self.dev.tx_streamer(channels, args)
    }
}

impl<T: AntennaControl> AntennaControl for Device<T> {
    fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        self.dev.antennas(direction, channel)
    }

    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        self.dev.antenna(direction, channel)
    }

    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error> {
        self.dev.set_antenna(direction, channel, name)
    }
}

impl<T: AgcControl> AgcControl for Device<T> {
    fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.dev.agc_available(direction, channel)
    }

    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.dev.agc_enabled(direction, channel)
    }

    fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error> {
        self.dev.set_agc_enabled(direction, channel, enabled)
    }
}

impl<T: GainControl> GainControl for Device<T> {
    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        self.dev.gain_elements(direction, channel)
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.dev.set_gain(direction, channel, gain)
    }

    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        self.dev.gain(direction, channel)
    }

    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.dev.gain_range(direction, channel)
    }

    fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        self.dev.set_gain_element(direction, channel, name, gain)
    }

    fn gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        self.dev.gain_element(direction, channel, name)
    }

    fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        self.dev.gain_element_range(direction, channel, name)
    }
}

impl<T: FrequencyControl> FrequencyControl for Device<T> {
    fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.dev.frequency_range(direction, channel)
    }

    fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.dev.frequency(direction, channel)
    }

    fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        args: Args,
    ) -> Result<(), Error> {
        self.dev.set_frequency(direction, channel, frequency, args)
    }

    fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        self.dev.frequency_components(direction, channel)
    }

    fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        self.dev.component_frequency_range(direction, channel, name)
    }

    fn component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error> {
        self.dev.component_frequency(direction, channel, name)
    }

    fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        self.dev
            .set_component_frequency(direction, channel, name, frequency)
    }
}

impl<T: SampleRateControl> SampleRateControl for Device<T> {
    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.dev.sample_rate(direction, channel)
    }

    fn set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        self.dev.set_sample_rate(direction, channel, rate)
    }

    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.dev.get_sample_rate_range(direction, channel)
    }
}

impl<T: BandwidthControl> BandwidthControl for Device<T> {
    fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.dev.bandwidth(direction, channel)
    }

    fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        self.dev.set_bandwidth(direction, channel, bw)
    }

    fn get_bandwidth_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.dev.get_bandwidth_range(direction, channel)
    }
}

impl<T: DcOffsetControl> DcOffsetControl for Device<T> {
    fn dc_offset_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.dev.dc_offset_available(direction, channel)
    }

    fn dc_offset_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.dev.dc_offset_enabled(direction, channel)
    }

    fn set_dc_offset_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error> {
        self.dev.set_dc_offset_enabled(direction, channel, enabled)
    }
}

impl DynDevice {
    /// Open the first discovered runtime-dispatched device.
    pub fn new() -> Result<Self, Error> {
        let registry = Registry::default();
        let descriptors = registry.probe(Args::new())?;
        let descriptor = descriptors.first().ok_or(Error::DeviceNotFound)?;
        registry.open(descriptor)
    }

    /// Open a runtime-dispatched device matching `args`.
    pub fn from_args<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        Registry::default().open_args(args)
    }

    /// Create a runtime-dispatched device from a concrete implementation.
    pub fn from_impl<T: DynDeviceBackend + Clone + 'static>(dev: T) -> Self {
        Self {
            inner: Arc::new(dev),
        }
    }

    /// Borrow the dynamic backend.
    pub fn as_backend(&self) -> &dyn DynDeviceBackend {
        self.inner.as_ref()
    }

    /// Try to downcast to a concrete device implementation.
    pub fn downcast_ref<D: DynDeviceBackend + 'static>(&self) -> Option<&D> {
        let backend: &dyn DynDeviceBackend = self.inner.as_ref();
        let any: &dyn Any = backend;
        any.downcast_ref::<D>()
    }

    /// Try to downcast mutably to a concrete device implementation.
    pub fn downcast_mut<D: DynDeviceBackend + 'static>(&mut self) -> Option<&mut D> {
        let backend: &mut dyn DynDeviceBackend = Arc::get_mut(&mut self.inner)?;
        let any: &mut dyn Any = backend;
        any.downcast_mut::<D>()
    }

    /// SDR [driver](Driver).
    pub fn driver(&self) -> Driver {
        self.inner.driver()
    }

    /// Identifier for the device, e.g. its serial.
    pub fn id(&self) -> Result<String, Error> {
        self.inner.id()
    }

    /// Device info that can be displayed to the user.
    pub fn info(&self) -> Result<Args, Error> {
        self.inner.info()
    }

    /// Number of supported channels.
    pub fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        self.inner.num_channels(direction)
    }

    /// Whether this device supports simultaneous reception and transmission.
    pub fn full_duplex(&self) -> Result<bool, Error> {
        self.inner.full_duplex()
    }

    /// Structured runtime capabilities for the device.
    pub fn capabilities(&self) -> Result<DeviceCapabilities, Error> {
        DeviceCapabilities::from_dyn(self.inner.as_ref())
    }

    /// RX channel handle.
    pub fn rx(&self, index: usize) -> Result<RxChannel<'_, Self>, Error> {
        ensure_channel(self, Direction::Rx, index)?;
        Ok(RxChannel::new(self, index))
    }

    /// TX channel handle.
    pub fn tx(&self, index: usize) -> Result<TxChannel<'_, Self>, Error> {
        ensure_channel(self, Direction::Tx, index)?;
        Ok(TxChannel::new(self, index))
    }

    /// Create an RX streamer.
    pub fn rx_streamer(&self, channels: &[usize]) -> Result<DynRxStreamer, Error> {
        self.rx_streamer_with_args(channels, Args::new())
    }

    /// Create an RX streamer, using `args`.
    pub fn rx_streamer_with_args<A: TryInto<Args>>(
        &self,
        channels: &[usize],
        args: A,
    ) -> Result<DynRxStreamer, Error> {
        for channel in channels {
            ensure_channel(self, Direction::Rx, *channel)?;
        }
        <Self as RxDevice>::rx_streamer(
            self,
            channels,
            args.try_into()
                .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?,
        )
    }

    /// Create a TX streamer.
    pub fn tx_streamer(&self, channels: &[usize]) -> Result<DynTxStreamer, Error> {
        self.tx_streamer_with_args(channels, Args::new())
    }

    /// Create a TX streamer, using `args`.
    pub fn tx_streamer_with_args<A: TryInto<Args>>(
        &self,
        channels: &[usize],
        args: A,
    ) -> Result<DynTxStreamer, Error> {
        for channel in channels {
            ensure_channel(self, Direction::Tx, *channel)?;
        }
        <Self as TxDevice>::tx_streamer(
            self,
            channels,
            args.try_into()
                .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?,
        )
    }
}

impl DeviceInfo for DynDevice {
    fn driver(&self) -> Driver {
        self.inner.driver()
    }
    fn id(&self) -> Result<String, Error> {
        self.inner.id()
    }
    fn info(&self) -> Result<Args, Error> {
        self.inner.info()
    }
    fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        self.inner.num_channels(direction)
    }
    fn full_duplex(&self) -> Result<bool, Error> {
        self.inner.full_duplex()
    }
}

impl RxDevice for DynDevice {
    type RxStreamer = DynRxStreamer;

    fn rx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::RxStreamer, Error> {
        self.inner
            .as_ref()
            .rx_device()
            .ok_or_else(|| Error::unsupported(Capability::RxStreaming))?
            .rx_streamer(channels, args)
    }
}

impl TxDevice for DynDevice {
    type TxStreamer = DynTxStreamer;

    fn tx_streamer(&self, channels: &[usize], args: Args) -> Result<Self::TxStreamer, Error> {
        self.inner
            .as_ref()
            .tx_device()
            .ok_or_else(|| Error::unsupported(Capability::TxStreaming))?
            .tx_streamer(channels, args)
    }
}

impl AntennaControl for DynDevice {
    fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        self.inner
            .as_ref()
            .antenna_control()
            .ok_or_else(|| Error::unsupported(Capability::Antenna))?
            .antennas(direction, channel)
    }

    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        self.inner
            .as_ref()
            .antenna_control()
            .ok_or_else(|| Error::unsupported(Capability::Antenna))?
            .antenna(direction, channel)
    }

    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error> {
        self.inner
            .as_ref()
            .antenna_control()
            .ok_or_else(|| Error::unsupported(Capability::Antenna))?
            .set_antenna(direction, channel, name)
    }
}

impl GainControl for DynDevice {
    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .gain_elements(direction, channel)
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .set_gain(direction, channel, gain)
    }

    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .gain(direction, channel)
    }

    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .gain_range(direction, channel)
    }

    fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .set_gain_element(direction, channel, name, gain)
    }

    fn gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .gain_element(direction, channel, name)
    }

    fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .gain_control()
            .ok_or_else(|| Error::unsupported(Capability::Gain))?
            .gain_element_range(direction, channel, name)
    }
}

impl FrequencyControl for DynDevice {
    fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .frequency_range(direction, channel)
    }

    fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .frequency(direction, channel)
    }

    fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        args: Args,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .set_frequency(direction, channel, frequency, args)
    }

    fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .frequency_components(direction, channel)
    }

    fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .component_frequency_range(direction, channel, name)
    }

    fn component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .component_frequency(direction, channel, name)
    }

    fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .frequency_control()
            .ok_or_else(|| Error::unsupported(Capability::Frequency))?
            .set_component_frequency(direction, channel, name, frequency)
    }
}

impl SampleRateControl for DynDevice {
    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.inner
            .as_ref()
            .sample_rate_control()
            .ok_or_else(|| Error::unsupported(Capability::SampleRate))?
            .sample_rate(direction, channel)
    }

    fn set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .sample_rate_control()
            .ok_or_else(|| Error::unsupported(Capability::SampleRate))?
            .set_sample_rate(direction, channel, rate)
    }

    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .sample_rate_control()
            .ok_or_else(|| Error::unsupported(Capability::SampleRate))?
            .get_sample_rate_range(direction, channel)
    }
}

impl BandwidthControl for DynDevice {
    fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.inner
            .as_ref()
            .bandwidth_control()
            .ok_or_else(|| Error::unsupported(Capability::Bandwidth))?
            .bandwidth(direction, channel)
    }

    fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        self.inner
            .as_ref()
            .bandwidth_control()
            .ok_or_else(|| Error::unsupported(Capability::Bandwidth))?
            .set_bandwidth(direction, channel, bw)
    }

    fn get_bandwidth_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.inner
            .as_ref()
            .bandwidth_control()
            .ok_or_else(|| Error::unsupported(Capability::Bandwidth))?
            .get_bandwidth_range(direction, channel)
    }
}

impl AgcControl for DynDevice {
    fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.inner
            .as_ref()
            .agc_control()
            .ok_or_else(|| Error::unsupported(Capability::Agc))?
            .agc_available(direction, channel)
    }

    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.inner
            .as_ref()
            .agc_control()
            .ok_or_else(|| Error::unsupported(Capability::Agc))?
            .agc_enabled(direction, channel)
    }

    fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .agc_control()
            .ok_or_else(|| Error::unsupported(Capability::Agc))?
            .set_agc_enabled(direction, channel, enabled)
    }
}

impl DcOffsetControl for DynDevice {
    fn dc_offset_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.inner
            .as_ref()
            .dc_offset_control()
            .ok_or_else(|| Error::unsupported(Capability::DcOffset))?
            .dc_offset_available(direction, channel)
    }

    fn dc_offset_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        self.inner
            .as_ref()
            .dc_offset_control()
            .ok_or_else(|| Error::unsupported(Capability::DcOffset))?
            .dc_offset_enabled(direction, channel)
    }

    fn set_dc_offset_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error> {
        self.inner
            .as_ref()
            .dc_offset_control()
            .ok_or_else(|| Error::unsupported(Capability::DcOffset))?
            .set_dc_offset_enabled(direction, channel, enabled)
    }
}

impl<T: DeviceInfo> Device<T> {
    /// RX channel handle.
    pub fn rx(&self, index: usize) -> Result<RxChannel<'_, T>, Error> {
        ensure_channel(&self.dev, Direction::Rx, index)?;
        Ok(RxChannel::new(&self.dev, index))
    }

    /// TX channel handle.
    pub fn tx(&self, index: usize) -> Result<TxChannel<'_, T>, Error> {
        ensure_channel(&self.dev, Direction::Tx, index)?;
        Ok(TxChannel::new(&self.dev, index))
    }
}

fn ensure_channel<T>(dev: &T, direction: Direction, channel: usize) -> Result<(), Error>
where
    T: DeviceInfo + ?Sized,
{
    let available = dev.num_channels(direction)?;
    if channel < available {
        Ok(())
    } else {
        Err(Error::invalid_channel(direction, channel, available))
    }
}

impl<T: RxDevice> Device<T> {
    /// Create an RX streamer over one or more RX channels.
    pub fn rx_streamer(&self, channels: &[usize]) -> Result<T::RxStreamer, Error> {
        self.rx_streamer_with_args(channels, Args::new())
    }

    /// Create an RX streamer over one or more RX channels, using `args`.
    pub fn rx_streamer_with_args(
        &self,
        channels: &[usize],
        args: Args,
    ) -> Result<T::RxStreamer, Error> {
        for channel in channels {
            ensure_channel(&self.dev, Direction::Rx, *channel)?;
        }
        self.dev.rx_streamer(channels, args)
    }
}

impl<T: TxDevice> Device<T> {
    /// Create a TX streamer over one or more TX channels.
    pub fn tx_streamer(&self, channels: &[usize]) -> Result<T::TxStreamer, Error> {
        self.tx_streamer_with_args(channels, Args::new())
    }

    /// Create a TX streamer over one or more TX channels, using `args`.
    pub fn tx_streamer_with_args(
        &self,
        channels: &[usize],
        args: Args,
    ) -> Result<T::TxStreamer, Error> {
        for channel in channels {
            ensure_channel(&self.dev, Direction::Tx, *channel)?;
        }
        self.dev.tx_streamer(channels, args)
    }
}

impl<'a, T: RxDevice + ?Sized> RxChannel<'a, T> {
    /// Create a single-channel RX streamer.
    pub fn streamer(&self) -> Result<T::RxStreamer, Error> {
        self.streamer_with_args(Args::new())
    }

    /// Create a single-channel RX streamer, using `args`.
    pub fn streamer_with_args(&self, args: Args) -> Result<T::RxStreamer, Error> {
        self.dev.rx_streamer(&[self.channel], args)
    }
}

impl<'a, T: TxDevice + ?Sized> TxChannel<'a, T> {
    /// Create a single-channel TX streamer.
    pub fn streamer(&self) -> Result<T::TxStreamer, Error> {
        self.streamer_with_args(Args::new())
    }

    /// Create a single-channel TX streamer, using `args`.
    pub fn streamer_with_args(&self, args: Args) -> Result<T::TxStreamer, Error> {
        self.dev.tx_streamer(&[self.channel], args)
    }
}

macro_rules! impl_channel_controls {
    ($channel:ident, $direction:expr) => {
        impl<'a, T: AntennaControl + ?Sized> $channel<'a, T> {
            /// Antenna control.
            pub fn antenna(&self) -> Antenna<'_, T> {
                Antenna::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: AgcControl + ?Sized> $channel<'a, T> {
            /// Automatic gain control.
            pub fn agc(&self) -> Agc<'_, T> {
                Agc::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: GainControl + ?Sized> $channel<'a, T> {
            /// Gain control.
            pub fn gain(&self) -> Gain<'_, T> {
                Gain::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: FrequencyControl + ?Sized> $channel<'a, T> {
            /// Frequency control.
            pub fn frequency(&self) -> Frequency<'_, T> {
                Frequency::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: SampleRateControl + ?Sized> $channel<'a, T> {
            /// Sample-rate control.
            pub fn sample_rate(&self) -> SampleRate<'_, T> {
                SampleRate::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: BandwidthControl + ?Sized> $channel<'a, T> {
            /// Bandwidth control.
            pub fn bandwidth(&self) -> Bandwidth<'_, T> {
                Bandwidth::new(self.dev, $direction, self.channel)
            }
        }

        impl<'a, T: DcOffsetControl + ?Sized> $channel<'a, T> {
            /// Automatic DC offset correction.
            pub fn dc_offset(&self) -> DcOffset<'_, T> {
                DcOffset::new(self.dev, $direction, self.channel)
            }
        }
    };
}

impl_channel_controls!(RxChannel, Direction::Rx);
impl_channel_controls!(TxChannel, Direction::Tx);

#[cfg(all(test, feature = "dummy"))]
mod tests {
    use super::*;
    use crate::ChannelControls;

    #[derive(Clone)]
    struct RxOnly;

    #[derive(Clone)]
    struct DcToggle(std::sync::Arc<std::sync::Mutex<bool>>);

    struct TestRxStreamer;

    impl DeviceInfo for RxOnly {
        fn driver(&self) -> Driver {
            Driver::Dummy
        }

        fn id(&self) -> Result<String, Error> {
            Ok("rx-only".to_string())
        }

        fn info(&self) -> Result<Args, Error> {
            Ok(Args::new())
        }

        fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
            match direction {
                Direction::Rx => Ok(1),
                Direction::Tx => Ok(0),
            }
        }

        fn full_duplex(&self) -> Result<bool, Error> {
            Ok(false)
        }
    }

    crate::impl_dyn_device_backend!(RxOnly => [rx]);

    impl RxDevice for RxOnly {
        type RxStreamer = TestRxStreamer;

        fn rx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::RxStreamer, Error> {
            match channels {
                &[0] => Ok(TestRxStreamer),
                _ => Err(Error::invalid_argument(
                    "channels",
                    "unsupported RX channel set",
                )),
            }
        }
    }

    impl DeviceInfo for DcToggle {
        fn driver(&self) -> Driver {
            Driver::Dummy
        }

        fn id(&self) -> Result<String, Error> {
            Ok("dc-toggle".to_string())
        }

        fn info(&self) -> Result<Args, Error> {
            Ok(Args::new())
        }

        fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
            match direction {
                Direction::Rx => Ok(1),
                Direction::Tx => Ok(0),
            }
        }

        fn full_duplex(&self) -> Result<bool, Error> {
            Ok(false)
        }
    }

    impl DcOffsetControl for DcToggle {
        fn dc_offset_available(
            &self,
            _direction: Direction,
            channel: usize,
        ) -> Result<bool, Error> {
            if channel == 0 {
                Ok(true)
            } else {
                Err(Error::invalid_channel(Direction::Rx, channel, 1))
            }
        }

        fn dc_offset_enabled(&self, _direction: Direction, channel: usize) -> Result<bool, Error> {
            if channel == 0 {
                Ok(*self.0.lock().unwrap())
            } else {
                Err(Error::invalid_channel(Direction::Rx, channel, 1))
            }
        }

        fn set_dc_offset_enabled(
            &self,
            _direction: Direction,
            channel: usize,
            enabled: bool,
        ) -> Result<(), Error> {
            if channel == 0 {
                *self.0.lock().unwrap() = enabled;
                Ok(())
            } else {
                Err(Error::invalid_channel(Direction::Rx, channel, 1))
            }
        }
    }

    impl crate::RxStreamer for TestRxStreamer {
        fn mtu(&self) -> Result<usize, Error> {
            Ok(1)
        }

        fn activate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
            Ok(())
        }

        fn deactivate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
            Ok(())
        }

        fn read(
            &mut self,
            _buffers: &mut [&mut [num_complex::Complex32]],
            _timeout_us: i64,
        ) -> Result<usize, Error> {
            Ok(0)
        }
    }

    #[test]
    fn dyn_device_reports_capabilities() {
        let dummy = crate::impls::Dummy::open(Args::new()).unwrap();
        let dev = DynDevice::from_impl(dummy);

        let capabilities = dev.capabilities().unwrap();

        assert!(dev.full_duplex().unwrap());
        assert!(capabilities.full_duplex);
        assert_eq!(capabilities.rx_channels.len(), 1);
        assert_eq!(capabilities.tx_channels.len(), 1);

        let rx0 = &capabilities.rx_channels[0];
        assert_eq!(rx0.channel, 0);
        assert_eq!(rx0.controls.antennas, Some(vec!["A".to_string()]));
        assert!(rx0.controls.agc);
        assert_eq!(rx0.controls.gain_elements, Some(vec!["RF".to_string()]));
        assert_eq!(
            rx0.controls.frequency_components,
            Some(vec!["freq".to_string()])
        );
        assert!(!rx0.controls.dc_offset);
    }

    #[test]
    fn antenna_handle_reports_ports_and_selected_port() {
        let dummy = crate::impls::Dummy::open(Args::new()).unwrap();
        let dev = Device::from_impl(dummy);
        let rx0 = dev.rx(0).unwrap();
        let antenna = rx0.antenna();

        assert_eq!(antenna.ports().unwrap(), vec![String::from("A")]);
        assert_eq!(antenna.selected().unwrap(), "A");
        antenna.select("A").unwrap();
    }

    #[test]
    fn typed_device_converts_to_dyn_device_and_downcasts() {
        let dummy = crate::impls::Dummy::open(Args::new()).unwrap();
        let dev = Device::from_impl(dummy);

        fn assert_typed_capabilities<D>(_dev: &D)
        where
            D: DeviceInfo
                + RxDevice
                + TxDevice
                + AntennaControl
                + AgcControl
                + GainControl
                + FrequencyControl
                + SampleRateControl
                + BandwidthControl,
        {
        }

        assert_typed_capabilities(&dev);
        assert!(dev.capabilities().unwrap().full_duplex);

        let dyn_dev = dev.to_dyn();

        dev.rx(0).unwrap().frequency().set(100.0e6).unwrap();
        assert_eq!(dyn_dev.rx(0).unwrap().frequency().value().unwrap(), 100.0e6);

        let mut dev = dev.into_dyn();

        assert_eq!(dev.driver(), Driver::Dummy);
        assert!(dev.downcast_ref::<crate::impls::Dummy>().is_some());
        assert!(dev.downcast_mut::<crate::impls::Dummy>().is_some());
    }

    #[test]
    fn agc_handle_controls_enabled_state() {
        let dummy = crate::impls::Dummy::open(Args::new()).unwrap();
        let dev = Device::from_impl(dummy);
        let rx0 = dev.rx(0).unwrap();
        let agc = rx0.agc();

        agc.enable().unwrap();
        assert!(agc.enabled().unwrap());

        agc.disable().unwrap();
        assert!(!agc.enabled().unwrap());
    }

    #[test]
    fn dc_offset_handle_controls_enabled_state() {
        let dev = Device::from_impl(DcToggle(std::sync::Arc::new(std::sync::Mutex::new(false))));
        let rx0 = RxChannel::new(&dev.dev, 0);
        let dc_offset = rx0.dc_offset();

        dc_offset.enable().unwrap();
        assert!(dc_offset.enabled().unwrap());

        dc_offset.disable().unwrap();
        assert!(!dc_offset.enabled().unwrap());
    }

    #[test]
    fn dyn_device_does_not_require_all_capabilities() {
        let dev = DynDevice::from_impl(RxOnly);

        let capabilities = dev.capabilities().unwrap();
        assert_eq!(capabilities.rx_channels.len(), 1);
        assert_eq!(capabilities.tx_channels.len(), 0);
        assert_eq!(
            capabilities.rx_channels[0].controls,
            ChannelControls::default()
        );

        assert!(dev.rx_streamer(&[0]).is_ok());
        assert!(matches!(
            dev.tx(0),
            Err(Error::InvalidChannel {
                direction: Direction::Tx,
                channel: 0,
                available: 0
            })
        ));
        let rx0 = dev.rx(0).unwrap();
        let agc = rx0.agc();
        assert!(matches!(
            agc.enabled(),
            Err(Error::Unsupported {
                capability: Capability::Agc,
                ..
            })
        ));
        let dc_offset = rx0.dc_offset();
        assert!(matches!(
            dc_offset.enabled(),
            Err(Error::Unsupported {
                capability: Capability::DcOffset,
                ..
            })
        ));
    }
}
