use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use futures::lock::Mutex as AsyncMutex;
use hydrasdr_rs::{
    AsyncF32RxStream, DecimationMode, Device as HydraSdrDevice, GainConfig, GainPreset, RfPort,
    SampleFormat,
};
use num_complex::Complex32;

use super::common::*;
use crate::Direction::*;
use crate::{
    async_compat::{timeout_from_micros, with_timeout, TimeoutResult},
    dev::AsyncTypedDeviceBackend,
    Args, AsyncAgcControl, AsyncAntennaControl, AsyncBandwidthControl, AsyncDeviceInfo,
    AsyncFrequencyControl, AsyncGainControl, AsyncRxDevice, AsyncSampleRateControl, Capability,
    Direction, Driver, Error, Range, RangeItem,
};

/// Asynchronous HydraSDR RFOne device backend.
#[derive(Clone)]
pub struct AsyncHydraSdr {
    session: Arc<AsyncMutex<AsyncHydraSession>>,
    serial: Option<u64>,
    inner: Arc<AsyncMutex<ReceiverState>>,
    active_rx_streams: Arc<AtomicUsize>,
    cleanup_needed: Arc<AtomicBool>,
}

/// HydraSDR RFOne asynchronous receive streamer.
pub struct AsyncHydraSdrRxStreamer {
    session: Arc<AsyncMutex<AsyncHydraSession>>,
    active_rx_streams: Arc<AtomicUsize>,
    cleanup_needed: Arc<AtomicBool>,
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
            Self::Device(device) => device.set_frequency_hz_async(frequency_hz).await,
            Self::Stream(stream) => stream.set_frequency_hz_async(frequency_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_sample_rate_hz(&mut self, sample_rate_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_sample_rate_hz_async(sample_rate_hz).await,
            Self::Stream(stream) => stream.set_sample_rate_hz_async(sample_rate_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_bandwidth_hz(&mut self, bandwidth_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_bandwidth_hz_async(bandwidth_hz).await,
            Self::Stream(stream) => stream.set_bandwidth_hz_async(bandwidth_hz).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_rf_port(&mut self, port: RfPort) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_rf_port_async(port).await,
            Self::Stream(stream) => stream.set_rf_port_async(port).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    async fn set_gain(&mut self, gain: GainConfig) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_gain_async(gain).await,
            Self::Stream(stream) => stream.set_gain_async(gain).await,
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }
}

struct ActivationClaim {
    active_rx_streams: Arc<AtomicUsize>,
    cleanup_needed: Arc<AtomicBool>,
    committed: bool,
}

impl ActivationClaim {
    fn acquire(
        active_rx_streams: &Arc<AtomicUsize>,
        cleanup_needed: &Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        active_rx_streams
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            active_rx_streams: Arc::clone(active_rx_streams),
            cleanup_needed: Arc::clone(cleanup_needed),
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
            self.active_rx_streams.store(0, Ordering::SeqCst);
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
        stream.finish().await.map_err(map_hydrasdr_error)?;
    }
    cleanup_needed.store(false, Ordering::SeqCst);
    Ok(())
}
impl AsyncHydraSdr {
    /// Return descriptors for detected HydraSDR RFOne devices asynchronously.
    pub async fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        let mut devs = Vec::new();
        for dev in HydraSdrDevice::list_async()
            .await
            .map_err(map_hydrasdr_error)?
        {
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
        let sample_rates = dev.sample_rates_async().await.unwrap_or_default();
        let bandwidths = dev.bandwidths_async().await.unwrap_or_default();
        let receiver_state = ReceiverState::from_device_info(dev.info(), sample_rates, bandwidths);

        Ok(Self {
            session: Arc::new(AsyncMutex::new(AsyncHydraSession::Device(Box::new(dev)))),
            serial,
            inner: Arc::new(AsyncMutex::new(receiver_state)),
            active_rx_streams: Arc::new(AtomicUsize::new(0)),
            cleanup_needed: Arc::new(AtomicBool::new(false)),
        })
    }

    fn ensure_rx_config_idle(&self) -> Result<(), Error> {
        if self.active_rx_streams.load(Ordering::SeqCst) == 0 {
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
        inner.agc = agc;
        inner.gain_config = manual_gain_config(&inner);
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

        let gain_update = match gain_type {
            GainType::Linearity => GainConfig::Preset(GainPreset::Linearity(gain.round() as u8)),
            GainType::Sensitivity => {
                GainConfig::Preset(GainPreset::Sensitivity(gain.round() as u8))
            }
            GainType::Lna => GainConfig::Manual {
                lna: Some(gain.round() as u8),
                mixer: None,
                vga: None,
                lna_agc: None,
                mixer_agc: None,
            },
            GainType::Mixer => GainConfig::Manual {
                lna: None,
                mixer: Some(gain.round() as u8),
                vga: None,
                lna_agc: None,
                mixer_agc: None,
            },
            GainType::Vga => GainConfig::Manual {
                lna: None,
                mixer: None,
                vga: Some(gain.round() as u8),
                lna_agc: None,
                mixer_agc: None,
            },
        };
        let mut session = self.lock_idle_session().await?;
        session.set_gain(gain_update).await?;
        let mut inner = self.inner.lock().await;
        if let Some(cached) = inner
            .gains
            .iter_mut()
            .find(|cached| cached.gain_type == gain_type)
        {
            cached.value = gain;
        }
        inner.gain_config = match gain_type {
            GainType::Linearity | GainType::Sensitivity => gain_update,
            GainType::Lna | GainType::Mixer | GainType::Vga => manual_gain_config(&inner),
        };
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
        Ok(Some(
            self.inner
                .lock()
                .await
                .gains
                .iter()
                .find(|cached| cached.gain_type == gain_type)
                .ok_or(Error::invalid_argument(
                    "hydrasdr",
                    "invalid HydraSDR argument",
                ))?
                .value,
        ))
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
        Ok(self
            .inner
            .lock()
            .await
            .gains
            .iter()
            .find(|cached| cached.gain_type == gain_type)
            .ok_or(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ))?
            .range
            .clone())
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
        if inner.sample_rates.is_empty() {
            Ok(Range::new(vec![RangeItem::Interval(
                DEFAULT_SAMPLE_RATE_MIN,
                u32::MAX as f64,
            )]))
        } else {
            Ok(Range::new(
                inner
                    .sample_rates
                    .iter()
                    .map(|rate| RangeItem::Value(*rate as f64))
                    .collect(),
            ))
        }
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
        if inner.bandwidths.is_empty() {
            Ok(Range::new(vec![RangeItem::Interval(
                DEFAULT_BANDWIDTH_MIN,
                u32::MAX as f64,
            )]))
        } else {
            Ok(Range::new(
                inner
                    .bandwidths
                    .iter()
                    .map(|bandwidth| RangeItem::Value(*bandwidth as f64))
                    .collect(),
            ))
        }
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
            Arc::clone(&self.session),
            Arc::clone(&self.active_rx_streams),
            Arc::clone(&self.cleanup_needed),
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
        session: Arc<AsyncMutex<AsyncHydraSession>>,
        active_rx_streams: Arc<AtomicUsize>,
        cleanup_needed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            session,
            active_rx_streams,
            cleanup_needed,
            iq_scratch: Vec::new(),
            active: false,
        }
    }
}

impl crate::AsyncRxStreamer for AsyncHydraSdrRxStreamer {
    async fn mtu(&self) -> Result<usize, Error> {
        Ok(MTU)
    }

    async fn activate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedActivation));
        }
        if self.active {
            return Ok(());
        }
        let claim = ActivationClaim::acquire(&self.active_rx_streams, &self.cleanup_needed)?;
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
        stream.finish().await.map_err(map_hydrasdr_error)?;
        self.cleanup_needed.store(false, Ordering::SeqCst);
        self.active_rx_streams.store(0, Ordering::SeqCst);
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
        // One MTU maps to one HydraSDR USB completion; keeping this read to one
        // completion makes timing out the wait cancellation-safe.
        let read_len = out.len().min(MTU);
        let read = match with_timeout(
            read_async_f32_stream(stream, &mut out[..read_len], &mut self.iq_scratch),
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
            self.active_rx_streams.store(0, Ordering::SeqCst);
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
            .open_async()
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
            .open_async()
            .await
            .map(|dev| (dev, Some(serial)))
            .map_err(map_hydrasdr_error),
        DeviceSelector::Index(index) => {
            let devices = HydraSdrDevice::list_async()
                .await
                .map_err(map_hydrasdr_error)?;
            let Some(info) = devices.get(index) else {
                return Err(Error::DeviceNotFound);
            };
            if let Some(serial) = info.serial {
                HydraSdrDevice::builder()
                    .serial(serial)
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open_async()
                    .await
                    .map(|dev| (dev, Some(serial)))
                    .map_err(map_hydrasdr_error)
            } else if index == 0 {
                HydraSdrDevice::builder()
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open_async()
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
        let active = Arc::new(AtomicUsize::new(0));
        let cleanup = Arc::new(AtomicBool::new(false));

        let claim = ActivationClaim::acquire(&active, &cleanup).expect("acquire stream claim");
        assert_eq!(active.load(Ordering::SeqCst), 1);
        drop(claim);

        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(cleanup.load(Ordering::SeqCst));
    }

    #[cfg(any(feature = "smol", feature = "tokio"))]
    #[test]
    fn committed_async_activation_keeps_exclusive_claim() {
        let active = Arc::new(AtomicUsize::new(0));
        let cleanup = Arc::new(AtomicBool::new(false));

        let claim = ActivationClaim::acquire(&active, &cleanup).expect("acquire stream claim");
        claim.commit();

        assert_eq!(active.load(Ordering::SeqCst), 1);
        assert!(!cleanup.load(Ordering::SeqCst));
        assert!(matches!(
            ActivationClaim::acquire(&active, &cleanup),
            Err(Error::Busy)
        ));
    }
}
