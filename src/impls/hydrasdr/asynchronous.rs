use std::sync::atomic::{AtomicBool, Ordering};

use futures::lock::Mutex as AsyncMutex;
use hydrasdr_rs::{
    AsyncF32RxStream, DecimationMode, Device as HydraSdrDevice, GainConfig, RfPort, SampleFormat,
};
use num_complex::Complex32;

use super::common::*;
#[cfg(target_arch = "wasm32")]
use crate::dev::WebUsbDeviceFilter;
use crate::Direction::*;
use crate::{
    async_compat::{timeout_from_micros, with_timeout, Shared, TimeoutResult},
    dev::AsyncTypedDeviceBackend,
    Args, AsyncAgcControl, AsyncAntennaControl, AsyncBandwidthControl, AsyncDeviceInfo,
    AsyncFrequencyControl, AsyncGainControl, AsyncRxDevice, AsyncSampleRateControl, Capability,
    Direction, Driver, Error, Range, RangeItem,
};

/// Asynchronous HydraSDR RFOne device backend.
#[derive(Clone)]
pub struct AsyncHydraSdr {
    session: Shared<AsyncMutex<AsyncHydraSession>>,
    serial: Option<u64>,
    inner: Shared<AsyncMutex<ReceiverState>>,
    rx_stream_active: Shared<AtomicBool>,
    cleanup_needed: Shared<AtomicBool>,
}

/// HydraSDR RFOne asynchronous receive streamer.
///
/// Activation uses one shared owned HydraSDR stream and persistent USB queue. Explicit
/// deactivation performs receiver-off cleanup. Dropping an active streamer leaves the session
/// available for cleanup by the next asynchronous device or stream operation.
#[must_use = "deactivate the HydraSDR stream before dropping it"]
pub struct AsyncHydraSdrRxStreamer {
    session: Shared<AsyncMutex<AsyncHydraSession>>,
    rx_stream_active: Shared<AtomicBool>,
    cleanup_needed: Shared<AtomicBool>,
    iq_scratch: Vec<(f32, f32)>,
    active: bool,
}

enum AsyncHydraSession {
    Device(Box<HydraSdrDevice>),
    Stream(Box<AsyncF32RxStream>),
    Disconnected,
}

impl AsyncHydraSession {
    fn ensure_stream(&mut self) -> Result<&mut AsyncF32RxStream, Error> {
        if matches!(self, Self::Device(_)) {
            let Self::Device(device) = std::mem::replace(self, Self::Disconnected) else {
                unreachable!();
            };
            *self = Self::Stream(Box::new((*device).into_async_f32_rx_stream()));
        }
        match self {
            Self::Stream(stream) => Ok(stream),
            Self::Disconnected => Err(Error::DeviceDisconnected),
            Self::Device(_) => unreachable!(),
        }
    }

    async fn set_frequency_hz(&mut self, frequency_hz: u64) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_frequency_hz(frequency_hz).await,
            Self::Stream(stream) => stream.set_frequency_hz(frequency_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_sample_rate_hz(&mut self, sample_rate_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_sample_rate_hz(sample_rate_hz).await,
            Self::Stream(stream) => stream.set_sample_rate_hz(sample_rate_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_bandwidth_hz(&mut self, bandwidth_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_bandwidth_hz(bandwidth_hz).await,
            Self::Stream(stream) => stream.set_bandwidth_hz(bandwidth_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_rf_port(&mut self, port: RfPort) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_rf_port(port).await,
            Self::Stream(stream) => stream.set_rf_port(port).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_gain(&mut self, gain: GainConfig) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_gain(gain).await,
            Self::Stream(stream) => stream.set_gain(gain).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }
}

struct ActivationClaim {
    rx_stream_active: Shared<AtomicBool>,
    cleanup_needed: Shared<AtomicBool>,
    committed: bool,
}

impl ActivationClaim {
    fn acquire(
        rx_stream_active: &Shared<AtomicBool>,
        cleanup_needed: &Shared<AtomicBool>,
    ) -> Result<Self, Error> {
        rx_stream_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            rx_stream_active: Shared::clone(rx_stream_active),
            cleanup_needed: Shared::clone(cleanup_needed),
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ActivationClaim {
    fn drop(&mut self) {
        if !self.committed {
            self.cleanup_needed.store(true, Ordering::SeqCst);
            self.rx_stream_active.store(false, Ordering::SeqCst);
        }
    }
}

async fn cleanup_abandoned_session(
    session: &mut AsyncHydraSession,
    cleanup_needed: &AtomicBool,
) -> Result<(), Error> {
    if !cleanup_needed.load(Ordering::SeqCst) {
        return Ok(());
    }
    if let AsyncHydraSession::Stream(stream) = session {
        stream.stop().await.map_err(map_hydrasdr_error)?;
    }
    cleanup_needed.store(false, Ordering::SeqCst);
    Ok(())
}

impl AsyncHydraSdr {
    /// Return descriptors for detected HydraSDR RFOne devices asynchronously.
    pub async fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        let mut devs = Vec::new();
        for dev in HydraSdrDevice::list().await.map_err(map_hydrasdr_error)? {
            devs.push(probe_args_from_info(dev));
        }
        Ok(devs)
    }

    /// Open a HydraSDR RFOne device from arguments asynchronously.
    pub async fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let selector = device_selector(&args)?;
        let (mut dev, serial) = open_selected_device_async(selector).await?;
        let sample_rates = dev.sample_rates().await.map_err(map_hydrasdr_error)?;
        let bandwidths = dev.bandwidths().await.unwrap_or_default();
        let receiver_state = ReceiverState::from_device_info(dev.info(), sample_rates, bandwidths);

        Ok(Self {
            session: Shared::new(AsyncMutex::new(AsyncHydraSession::Device(Box::new(dev)))),
            serial,
            inner: Shared::new(AsyncMutex::new(receiver_state)),
            rx_stream_active: Shared::new(AtomicBool::new(false)),
            cleanup_needed: Shared::new(AtomicBool::new(false)),
        })
    }

    fn ensure_rx_config_idle(&self) -> Result<(), Error> {
        if !self.rx_stream_active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::Busy)
        }
    }

    async fn lock_idle_session(
        &self,
    ) -> Result<futures::lock::MutexGuard<'_, AsyncHydraSession>, Error> {
        self.ensure_rx_config_idle()?;
        let mut session = self.session.lock().await;
        self.ensure_rx_config_idle()?;
        cleanup_abandoned_session(&mut session, &self.cleanup_needed).await?;
        Ok(session)
    }
}

impl AsyncHydraSdr {
    fn driver(&self) -> Driver {
        Driver::HydraSdr
    }

    async fn id(&self) -> Result<String, Error> {
        self.serial
            .map(|serial| serial.to_string())
            .ok_or_else(|| Error::unsupported(Capability::DeviceId))
    }

    async fn info(&self) -> Result<Args, Error> {
        let mut args = Args::default();
        args.set("driver", "hydrasdr");
        args.set("serial", self.id().await?);
        Ok(args)
    }

    async fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        match direction {
            Rx => Ok(1),
            Tx => Ok(0),
        }
    }

    async fn full_duplex(&self) -> Result<bool, Error> {
        Ok(false)
    }

    async fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(["ANT", "CABLE1", "CABLE2"]
            .into_iter()
            .map(str::to_string)
            .collect())
    }

    async fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        check_rx(direction, channel)?;
        Ok(self.inner.lock().await.antenna.to_string())
    }

    async fn set_antenna(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let (name, port) = antenna_port(name).ok_or(Error::invalid_argument(
            "hydrasdr",
            "invalid HydraSDR argument",
        ))?;
        let mut session = self.lock_idle_session().await?;
        session.set_rf_port(port).await?;
        self.inner.lock().await.antenna = name;
        Ok(())
    }

    async fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        Ok(true)
    }

    async fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        agc: bool,
    ) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let gain = GainConfig::Manual {
            lna: None,
            mixer: None,
            vga: None,
            lna_agc: Some(agc),
            mixer_agc: Some(agc),
        };
        let mut session = self.lock_idle_session().await?;
        session.set_gain(gain).await?;
        let mut inner = self.inner.lock().await;
        inner.set_agc_cached(agc);
        Ok(())
    }

    async fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        Ok(self.inner.lock().await.agc)
    }

    async fn gain_elements(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(self
            .inner
            .lock()
            .await
            .gains
            .iter()
            .map(|gain| gain.name.to_string())
            .collect())
    }

    async fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.set_gain_element(direction, channel, "LINEARITY", gain)
            .await
    }

    async fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        self.gain_element(direction, channel, "LINEARITY").await
    }

    async fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.gain_element_range(direction, channel, "LINEARITY")
            .await
    }

    async fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let gain_type = gain_type(name).ok_or(Error::invalid_argument(
            "hydrasdr",
            "invalid HydraSDR argument",
        ))?;
        let range = self.gain_element_range(direction, channel, name).await?;
        if !range.contains(gain) {
            return Err(Error::out_of_range("gain", range, gain));
        }

        let gain_update = gain_type.update(gain);
        let mut session = self.lock_idle_session().await?;
        session.set_gain(gain_update).await?;
        let mut inner = self.inner.lock().await;
        inner.set_gain_cached(gain_type, gain);
        Ok(())
    }

    async fn gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        check_rx(direction, channel)?;
        let gain_type = gain_type(name).ok_or(Error::invalid_argument(
            "hydrasdr",
            "invalid HydraSDR argument",
        ))?;
        Ok(Some(self.inner.lock().await.gain_value(gain_type).ok_or(
            Error::invalid_argument("hydrasdr", "invalid HydraSDR argument"),
        )?))
    }

    async fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        let gain_type = gain_type(name).ok_or(Error::invalid_argument(
            "hydrasdr",
            "invalid HydraSDR argument",
        ))?;
        self.inner
            .lock()
            .await
            .gain_range(gain_type)
            .ok_or(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ))
    }

    async fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.component_frequency_range(direction, channel, "TUNER")
            .await
    }

    async fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.component_frequency(direction, channel, "TUNER").await
    }

    async fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        _args: Args,
    ) -> Result<(), Error> {
        self.set_component_frequency(direction, channel, "TUNER", frequency)
            .await
    }

    async fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(vec!["TUNER".to_string()])
    }

    async fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        if name == "TUNER" {
            let inner = self.inner.lock().await;
            Ok(Range::new(vec![RangeItem::Interval(
                inner.min_frequency,
                inner.max_frequency,
            )]))
        } else {
            Err(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ))
        }
    }

    async fn component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        if name != "TUNER" {
            return Err(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ));
        }
        self.inner
            .lock()
            .await
            .frequency
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    async fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        let range = self
            .component_frequency_range(direction, channel, name)
            .await?;
        if !range.contains(frequency) {
            return Err(Error::out_of_range("frequency", range, frequency));
        }
        let mut session = self.lock_idle_session().await?;
        session.set_frequency_hz(frequency as u64).await?;
        self.inner.lock().await.frequency = Some(frequency);
        Ok(())
    }

    async fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner
            .lock()
            .await
            .sample_rate
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    async fn set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        let range = self.get_sample_rate_range(direction, channel).await?;
        if !range.contains(rate) {
            return Err(Error::out_of_range("sample_rate", range, rate));
        }
        let mut session = self.lock_idle_session().await?;
        session.set_sample_rate_hz(rate as u32).await?;
        self.inner.lock().await.sample_rate = Some(rate);
        Ok(())
    }

    async fn get_sample_rate_range(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        let inner = self.inner.lock().await;
        sample_rate_range(&inner.sample_rates)
    }

    async fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner
            .lock()
            .await
            .bandwidth
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    async fn set_bandwidth(
        &self,
        direction: Direction,
        channel: usize,
        bw: f64,
    ) -> Result<(), Error> {
        let range = self.get_bandwidth_range(direction, channel).await?;
        if !range.contains(bw) {
            return Err(Error::out_of_range("bandwidth", range, bw));
        }
        let mut session = self.lock_idle_session().await?;
        session.set_bandwidth_hz(bw as u32).await?;
        self.inner.lock().await.bandwidth = Some(bw);
        Ok(())
    }

    async fn get_bandwidth_range(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        let inner = self.inner.lock().await;
        bandwidth_range(&inner.bandwidths)
    }
}

impl AsyncDeviceInfo for AsyncHydraSdr {
    fn driver(&self) -> Driver {
        AsyncHydraSdr::driver(self)
    }

    async fn async_id(&self) -> Result<String, Error> {
        AsyncHydraSdr::id(self).await
    }

    async fn async_info(&self) -> Result<Args, Error> {
        AsyncHydraSdr::info(self).await
    }

    async fn async_num_channels(&self, direction: Direction) -> Result<usize, Error> {
        AsyncHydraSdr::num_channels(self, direction).await
    }

    async fn async_full_duplex(&self) -> Result<bool, Error> {
        AsyncHydraSdr::full_duplex(self).await
    }
}

crate::impl_dyn_async_device_backend!(
    AsyncHydraSdr => [rx, antenna, agc, gain, frequency, sample_rate, bandwidth]
);

impl AsyncRxDevice for AsyncHydraSdr {
    type RxStreamer = AsyncHydraSdrRxStreamer;

    async fn async_rx_streamer(
        &self,
        channels: &[usize],
        _args: Args,
    ) -> Result<Self::RxStreamer, Error> {
        if channels != [0] {
            return Err(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ));
        }
        self.ensure_rx_config_idle()?;
        Ok(AsyncHydraSdrRxStreamer::new(
            Shared::clone(&self.session),
            Shared::clone(&self.rx_stream_active),
            Shared::clone(&self.cleanup_needed),
        ))
    }
}

impl AsyncAntennaControl for AsyncHydraSdr {
    async fn async_antennas(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        AsyncHydraSdr::antennas(self, direction, channel).await
    }

    async fn async_antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        AsyncHydraSdr::antenna(self, direction, channel).await
    }

    async fn async_set_antenna(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_antenna(self, direction, channel, name).await
    }
}

impl AsyncAgcControl for AsyncHydraSdr {
    async fn async_agc_available(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<bool, Error> {
        AsyncHydraSdr::agc_available(self, direction, channel).await
    }

    async fn async_agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        AsyncHydraSdr::agc_enabled(self, direction, channel).await
    }

    async fn async_set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        enabled: bool,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_agc_enabled(self, direction, channel, enabled).await
    }
}

impl AsyncGainControl for AsyncHydraSdr {
    async fn async_gain_elements(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        AsyncHydraSdr::gain_elements(self, direction, channel).await
    }

    async fn async_set_gain(
        &self,
        direction: Direction,
        channel: usize,
        gain: f64,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_gain(self, direction, channel, gain).await
    }

    async fn async_gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        AsyncHydraSdr::gain(self, direction, channel).await
    }

    async fn async_gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        AsyncHydraSdr::gain_range(self, direction, channel).await
    }

    async fn async_set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_gain_element(self, direction, channel, name, gain).await
    }

    async fn async_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        AsyncHydraSdr::gain_element(self, direction, channel, name).await
    }

    async fn async_gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        AsyncHydraSdr::gain_element_range(self, direction, channel, name).await
    }
}

impl AsyncFrequencyControl for AsyncHydraSdr {
    async fn async_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Range, Error> {
        AsyncHydraSdr::frequency_range(self, direction, channel).await
    }

    async fn async_frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        AsyncHydraSdr::frequency(self, direction, channel).await
    }

    async fn async_set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        args: Args,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_frequency(self, direction, channel, frequency, args).await
    }

    async fn async_frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        AsyncHydraSdr::frequency_components(self, direction, channel).await
    }

    async fn async_component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        AsyncHydraSdr::component_frequency_range(self, direction, channel, name).await
    }

    async fn async_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error> {
        AsyncHydraSdr::component_frequency(self, direction, channel, name).await
    }

    async fn async_set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_component_frequency(self, direction, channel, name, frequency).await
    }
}

impl AsyncSampleRateControl for AsyncHydraSdr {
    async fn async_sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        AsyncHydraSdr::sample_rate(self, direction, channel).await
    }

    async fn async_set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_sample_rate(self, direction, channel, rate).await
    }

    async fn async_get_sample_rate_range(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Range, Error> {
        AsyncHydraSdr::get_sample_rate_range(self, direction, channel).await
    }
}

impl AsyncBandwidthControl for AsyncHydraSdr {
    async fn async_bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        AsyncHydraSdr::bandwidth(self, direction, channel).await
    }

    async fn async_set_bandwidth(
        &self,
        direction: Direction,
        channel: usize,
        bw: f64,
    ) -> Result<(), Error> {
        AsyncHydraSdr::set_bandwidth(self, direction, channel, bw).await
    }

    async fn async_get_bandwidth_range(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Range, Error> {
        AsyncHydraSdr::get_bandwidth_range(self, direction, channel).await
    }
}

impl AsyncHydraSdrRxStreamer {
    fn new(
        session: Shared<AsyncMutex<AsyncHydraSession>>,
        rx_stream_active: Shared<AtomicBool>,
        cleanup_needed: Shared<AtomicBool>,
    ) -> Self {
        Self {
            session,
            rx_stream_active,
            cleanup_needed,
            iq_scratch: Vec::new(),
            active: false,
        }
    }
}

impl crate::AsyncRxStreamer for AsyncHydraSdrRxStreamer {
    async fn mtu(&self) -> Result<usize, Error> {
        Ok(F32_RX_MTU)
    }

    async fn activate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedActivation));
        }
        if self.active {
            return Ok(());
        }
        let claim = ActivationClaim::acquire(&self.rx_stream_active, &self.cleanup_needed)?;
        let mut session = self.session.lock().await;
        cleanup_abandoned_session(&mut session, &self.cleanup_needed).await?;
        session
            .ensure_stream()?
            .start()
            .await
            .map_err(map_hydrasdr_error)?;
        self.cleanup_needed.store(false, Ordering::SeqCst);
        self.active = true;
        claim.commit();
        Ok(())
    }

    async fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedDeactivation));
        }
        if !self.active {
            return Ok(());
        }
        let mut session = self.session.lock().await;
        let AsyncHydraSession::Stream(stream) = &mut *session else {
            return Err(Error::DeviceDisconnected);
        };
        stream.stop().await.map_err(map_hydrasdr_error)?;
        self.cleanup_needed.store(false, Ordering::SeqCst);
        self.rx_stream_active.store(false, Ordering::SeqCst);
        self.active = false;
        Ok(())
    }

    async fn read<'a>(
        &'a mut self,
        buffers: &'a mut [&'a mut [Complex32]],
        timeout_us: i64,
    ) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::StreamInactive);
        }
        crate::streamer::expect_buffer_count(buffers.len(), 1)?;
        if buffers[0].is_empty() {
            return Ok(0);
        }

        let out = &mut buffers[0];
        let mut session = self.session.lock().await;
        let AsyncHydraSession::Stream(stream) = &mut *session else {
            return Err(Error::DeviceDisconnected);
        };
        let read = match with_timeout(
            read_async_f32_stream(stream, out, &mut self.iq_scratch),
            timeout_from_micros(timeout_us),
        )
        .await
        {
            TimeoutResult::Completed(read) => read?,
            TimeoutResult::TimedOut => 0,
        };
        Ok(read)
    }
}

impl Drop for AsyncHydraSdrRxStreamer {
    fn drop(&mut self) {
        if self.active {
            self.cleanup_needed.store(true, Ordering::SeqCst);
            self.rx_stream_active.store(false, Ordering::SeqCst);
        }
        self.active = false;
    }
}

async fn read_async_f32_stream(
    stream: &mut AsyncF32RxStream,
    out: &mut [Complex32],
    iq_scratch: &mut Vec<(f32, f32)>,
) -> Result<usize, Error> {
    iq_scratch.resize(out.len(), (0.0, 0.0));
    let read = stream.read(iq_scratch).await.map_err(map_hydrasdr_error)?;
    for (dst, (i, q)) in out.iter_mut().take(read).zip(iq_scratch.iter().copied()) {
        *dst = Complex32::new(i, q);
    }
    Ok(read)
}

impl AsyncTypedDeviceBackend for AsyncHydraSdr {
    fn driver() -> Driver {
        Driver::HydraSdr
    }

    #[cfg(target_arch = "wasm32")]
    fn webusb_filters(args: &Args) -> Result<Vec<WebUsbDeviceFilter>, Error> {
        let serial = match device_selector(args)? {
            DeviceSelector::Serial(serial) => Some(format!("HYDRASDR SN:{serial:016X}")),
            DeviceSelector::First | DeviceSelector::Index(_) => None,
        };
        Ok([(0x1d50, 0x60a1), (0x38af, 0x0001)]
            .into_iter()
            .map(|(vendor_id, product_id)| {
                let filter = WebUsbDeviceFilter::new().with_vendor_product(vendor_id, product_id);
                if let Some(serial) = &serial {
                    filter.with_serial_number(serial.clone())
                } else {
                    filter
                }
            })
            .collect())
    }

    async fn async_probe(args: &Args) -> Result<Vec<Args>, Error> {
        Self::probe(args).await
    }

    async fn async_open(args: &Args) -> Result<Self, Error> {
        Self::open(args.clone()).await
    }
}

async fn open_selected_device_async(
    selector: DeviceSelector,
) -> Result<(HydraSdrDevice, Option<u64>), Error> {
    match selector {
        DeviceSelector::First => HydraSdrDevice::builder()
            .sample_format(SampleFormat::F32Iq)
            .decimation_mode(DecimationMode::HighDefinition)
            .open()
            .await
            .map(|dev| {
                let serial = dev.info().serial;
                (dev, serial)
            })
            .map_err(map_hydrasdr_error),
        DeviceSelector::Serial(serial) => HydraSdrDevice::builder()
            .serial(serial)
            .sample_format(SampleFormat::F32Iq)
            .decimation_mode(DecimationMode::HighDefinition)
            .open()
            .await
            .map(|dev| (dev, Some(serial)))
            .map_err(map_hydrasdr_error),
        DeviceSelector::Index(index) => {
            let devices = HydraSdrDevice::list().await.map_err(map_hydrasdr_error)?;
            let Some(info) = devices.get(index) else {
                return Err(Error::DeviceNotFound);
            };
            if let Some(serial) = info.serial {
                HydraSdrDevice::builder()
                    .serial(serial)
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open()
                    .await
                    .map(|dev| (dev, Some(serial)))
                    .map_err(map_hydrasdr_error)
            } else if index == 0 {
                HydraSdrDevice::builder()
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open()
                    .await
                    .map(|dev| {
                        let serial = dev.info().serial;
                        (dev, serial)
                    })
                    .map_err(map_hydrasdr_error)
            } else {
                Err(Error::DeviceNotFound)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(feature = "smol", feature = "tokio"))]
    #[test]
    fn canceled_async_activation_releases_claim_and_requests_cleanup() {
        let active = Shared::new(AtomicBool::new(false));
        let cleanup = Shared::new(AtomicBool::new(false));

        let claim = ActivationClaim::acquire(&active, &cleanup).expect("acquire stream claim");
        assert!(active.load(Ordering::SeqCst));
        drop(claim);

        assert!(!active.load(Ordering::SeqCst));
        assert!(cleanup.load(Ordering::SeqCst));
    }

    #[cfg(any(feature = "smol", feature = "tokio"))]
    #[test]
    fn committed_async_activation_keeps_exclusive_claim() {
        let active = Shared::new(AtomicBool::new(false));
        let cleanup = Shared::new(AtomicBool::new(false));

        let claim = ActivationClaim::acquire(&active, &cleanup).expect("acquire stream claim");
        claim.commit();

        assert!(active.load(Ordering::SeqCst));
        assert!(!cleanup.load(Ordering::SeqCst));
        assert!(matches!(
            ActivationClaim::acquire(&active, &cleanup),
            Err(Error::Busy)
        ));
    }
}
