use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use hydrasdr_rs::{
    DecimationMode, Device as HydraSdrDevice, F32RxStream, GainConfig, GainPreset, MaybeFuture,
    RfPort, SampleFormat,
};
use num_complex::Complex32;

use super::common::*;
use crate::Direction::*;
use crate::{
    AgcControl, AntennaControl, Args, BandwidthControl, Capability, DeviceInfo, Direction, Driver,
    Error, FrequencyControl, GainControl, Range, RangeItem, RxDevice, SampleRateControl,
};

/// HydraSDR RFOne device backend.
#[derive(Clone)]
pub struct HydraSdr {
    session: Arc<Mutex<SyncHydraSession>>,
    serial: Option<u64>,
    inner: Arc<Mutex<ReceiverState>>,
    rx_stream_active: Arc<AtomicBool>,
}
/// HydraSDR RFOne receive streamer.
pub struct RxStreamer {
    session: Arc<Mutex<SyncHydraSession>>,
    rx_stream_active: Arc<AtomicBool>,
    iq_scratch: Vec<(f32, f32)>,
    active: bool,
}

/// Placeholder transmit streamer for unsupported TX operations.
pub struct TxDummy;

enum SyncHydraSession {
    Device(Box<HydraSdrDevice>),
    Stream(Box<F32RxStream>),
    Disconnected,
}

impl SyncHydraSession {
    fn ensure_stream(&mut self) -> Result<&mut F32RxStream, Error> {
        if matches!(self, Self::Device(_)) {
            let Self::Device(device) = std::mem::replace(self, Self::Disconnected) else {
                unreachable!();
            };
            *self = Self::Stream(Box::new((*device).into_f32_rx_stream()));
        }
        match self {
            Self::Stream(stream) => Ok(stream),
            Self::Disconnected => Err(Error::DeviceDisconnected),
            Self::Device(_) => unreachable!(),
        }
    }

    fn stop_stream(&mut self) -> Result<(), Error> {
        if let Self::Stream(stream) = self {
            stream.stop().map_err(map_hydrasdr_error)?;
        }
        Ok(())
    }

    fn set_frequency_hz(&mut self, frequency_hz: u64) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_frequency_hz(frequency_hz).wait(),
            Self::Stream(stream) => stream.set_frequency_hz(frequency_hz),
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    fn set_sample_rate_hz(&mut self, sample_rate_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_sample_rate_hz(sample_rate_hz).wait(),
            Self::Stream(stream) => stream.set_sample_rate_hz(sample_rate_hz),
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    fn set_bandwidth_hz(&mut self, bandwidth_hz: u32) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_bandwidth_hz(bandwidth_hz).wait(),
            Self::Stream(stream) => stream.set_bandwidth_hz(bandwidth_hz),
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    fn set_rf_port(&mut self, port: RfPort) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_rf_port(port).wait(),
            Self::Stream(stream) => stream.set_rf_port(port),
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }

    fn set_gain(&mut self, gain: GainConfig) -> Result<(), Error> {
        match self {
            Self::Device(device) => device.set_gain(gain).wait(),
            Self::Stream(stream) => stream.set_gain(gain),
            Self::Disconnected => return Err(Error::DeviceDisconnected),
        }
        .map_err(map_hydrasdr_error)
    }
}

struct SyncActivationClaim {
    rx_stream_active: Arc<AtomicBool>,
    committed: bool,
}

impl SyncActivationClaim {
    fn acquire(rx_stream_active: &Arc<AtomicBool>) -> Result<Self, Error> {
        rx_stream_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            rx_stream_active: Arc::clone(rx_stream_active),
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SyncActivationClaim {
    fn drop(&mut self) {
        if !self.committed {
            self.rx_stream_active.store(false, Ordering::SeqCst);
        }
    }
}

impl HydraSdr {
    /// Return descriptors for detected HydraSDR RFOne devices.
    pub fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        let mut devs = Vec::new();
        for dev in HydraSdrDevice::list().wait().map_err(map_hydrasdr_error)? {
            devs.push(probe_args_from_info(dev));
        }
        Ok(devs)
    }

    /// Open a HydraSDR RFOne device from arguments.
    pub fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let selector = device_selector(&args)?;
        let (mut dev, serial) = open_selected_device(selector)?;
        let sample_rates = dev.sample_rates().wait().map_err(map_hydrasdr_error)?;
        let bandwidths = dev.bandwidths().wait().unwrap_or_default();
        let receiver_state = ReceiverState::from_device_info(dev.info(), sample_rates, bandwidths);

        Ok(Self {
            session: Arc::new(Mutex::new(SyncHydraSession::Device(Box::new(dev)))),
            serial,
            inner: Arc::new(Mutex::new(receiver_state)),
            rx_stream_active: Arc::new(AtomicBool::new(false)),
        })
    }

    fn ensure_rx_config_idle(&self) -> Result<(), Error> {
        if !self.rx_stream_active.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Error::Busy)
        }
    }

    fn lock_idle_session(&self) -> Result<MutexGuard<'_, SyncHydraSession>, Error> {
        self.ensure_rx_config_idle()?;
        let mut session = self.session.lock().unwrap();
        self.ensure_rx_config_idle()?;
        session.stop_stream()?;
        Ok(session)
    }
}

impl HydraSdr {
    fn driver(&self) -> Driver {
        Driver::HydraSdr
    }

    fn id(&self) -> Result<String, Error> {
        if let Some(serial) = self.serial {
            return Ok(serial.to_string());
        }

        Err(Error::unsupported(Capability::DeviceId))
    }

    fn info(&self) -> Result<Args, Error> {
        let mut args = Args::default();
        args.set("driver", "hydrasdr");
        args.set("serial", self.id()?);
        Ok(args)
    }

    fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        match direction {
            Rx => Ok(1),
            Tx => Ok(0),
        }
    }

    fn full_duplex(&self) -> Result<bool, Error> {
        Ok(false)
    }

    fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(["ANT", "CABLE1", "CABLE2"]
            .into_iter()
            .map(str::to_string)
            .collect())
    }

    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        check_rx(direction, channel)?;
        Ok(self.inner.lock().unwrap().antenna.to_string())
    }

    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error> {
        check_rx(direction, channel)?;
        self.ensure_rx_config_idle()?;
        let (name, port) = antenna_port(name).ok_or(Error::invalid_argument(
            "hydrasdr",
            "invalid HydraSDR argument",
        ))?;
        {
            self.lock_idle_session()?.set_rf_port(port)?;
        }
        self.inner.lock().unwrap().antenna = name;
        Ok(())
    }

    fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        Ok(true)
    }

    fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        agc: bool,
    ) -> Result<(), Error> {
        check_rx(direction, channel)?;
        self.ensure_rx_config_idle()?;
        let gain = GainConfig::Manual {
            lna: None,
            mixer: None,
            vga: None,
            lna_agc: Some(agc),
            mixer_agc: Some(agc),
        };
        {
            self.lock_idle_session()?.set_gain(gain)?;
        }
        let mut inner = self.inner.lock().unwrap();
        inner.agc = agc;
        inner.gain_config = manual_gain_config(&inner);
        Ok(())
    }

    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        Ok(self.inner.lock().unwrap().agc)
    }

    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(self
            .inner
            .lock()
            .unwrap()
            .gains
            .iter()
            .map(|gain| gain.name.to_string())
            .collect())
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.set_gain_element(direction, channel, "LINEARITY", gain)
    }

    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        self.gain_element(direction, channel, "LINEARITY")
    }

    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.gain_element_range(direction, channel, "LINEARITY")
    }

    fn set_gain_element(
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
        let range = self.gain_element_range(direction, channel, name)?;
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
        self.ensure_rx_config_idle()?;
        {
            self.lock_idle_session()?.set_gain(gain_update)?;
        }
        let mut inner = self.inner.lock().unwrap();
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

    fn gain_element(
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
                .unwrap()
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

    fn gain_element_range(
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
            .unwrap()
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

    fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        self.component_frequency_range(direction, channel, "TUNER")
    }

    fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        self.component_frequency(direction, channel, "TUNER")
    }

    fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        _args: Args,
    ) -> Result<(), Error> {
        self.set_component_frequency(direction, channel, "TUNER", frequency)
    }

    fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(vec!["TUNER".to_string()])
    }

    fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        if name == "TUNER" {
            let inner = self.inner.lock().unwrap();
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

    fn component_frequency(
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
            .unwrap()
            .frequency
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        let range = self.component_frequency_range(direction, channel, name)?;
        if !range.contains(frequency) {
            return Err(Error::out_of_range("frequency", range, frequency));
        }
        self.ensure_rx_config_idle()?;
        {
            self.lock_idle_session()?
                .set_frequency_hz(frequency as u64)?;
        }
        self.inner.lock().unwrap().frequency = Some(frequency);
        Ok(())
    }

    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner
            .lock()
            .unwrap()
            .sample_rate
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    fn set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        let range = self.get_sample_rate_range(direction, channel)?;
        if !range.contains(rate) {
            return Err(Error::out_of_range("sample_rate", range, rate));
        }
        self.ensure_rx_config_idle()?;
        {
            self.lock_idle_session()?.set_sample_rate_hz(rate as u32)?;
        }
        self.inner.lock().unwrap().sample_rate = Some(rate);
        Ok(())
    }

    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        let rates = &self.inner.lock().unwrap().sample_rates;
        if rates.is_empty() {
            Ok(Range::new(vec![RangeItem::Interval(
                DEFAULT_SAMPLE_RATE_MIN,
                u32::MAX as f64,
            )]))
        } else {
            Ok(Range::new(
                rates
                    .iter()
                    .map(|rate| RangeItem::Value(*rate as f64))
                    .collect(),
            ))
        }
    }

    fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner
            .lock()
            .unwrap()
            .bandwidth
            .ok_or(Error::unsupported(Capability::DriverOperation))
    }

    fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        let range = self.get_bandwidth_range(direction, channel)?;
        if !range.contains(bw) {
            return Err(Error::out_of_range("bandwidth", range, bw));
        }
        self.ensure_rx_config_idle()?;
        {
            self.lock_idle_session()?.set_bandwidth_hz(bw as u32)?;
        }
        self.inner.lock().unwrap().bandwidth = Some(bw);
        Ok(())
    }

    fn get_bandwidth_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        let bandwidths = &self.inner.lock().unwrap().bandwidths;
        bandwidth_range(bandwidths)
    }
}

impl DeviceInfo for HydraSdr {
    fn driver(&self) -> Driver {
        HydraSdr::driver(self)
    }

    fn id(&self) -> Result<String, Error> {
        HydraSdr::id(self)
    }

    fn info(&self) -> Result<Args, Error> {
        HydraSdr::info(self)
    }

    fn num_channels(&self, direction: Direction) -> Result<usize, Error> {
        HydraSdr::num_channels(self, direction)
    }

    fn full_duplex(&self) -> Result<bool, Error> {
        HydraSdr::full_duplex(self)
    }
}

crate::impl_dyn_device_backend!(
    HydraSdr => [rx, antenna, agc, gain, frequency, sample_rate, bandwidth]
);
crate::registry::impl_typed_device_backend!(HydraSdr, Driver::HydraSdr);

impl RxDevice for HydraSdr {
    type RxStreamer = RxStreamer;

    fn rx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::RxStreamer, Error> {
        if channels != [0] {
            return Err(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ));
        }
        self.ensure_rx_config_idle()?;
        Ok(RxStreamer::new(
            Arc::clone(&self.session),
            Arc::clone(&self.rx_stream_active),
        ))
    }
}

impl AntennaControl for HydraSdr {
    fn antennas(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        HydraSdr::antennas(self, direction, channel)
    }

    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        HydraSdr::antenna(self, direction, channel)
    }

    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error> {
        HydraSdr::set_antenna(self, direction, channel, name)
    }
}

impl AgcControl for HydraSdr {
    fn agc_available(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        HydraSdr::agc_available(self, direction, channel)
    }

    fn set_agc_enabled(
        &self,
        direction: Direction,
        channel: usize,
        agc: bool,
    ) -> Result<(), Error> {
        HydraSdr::set_agc_enabled(self, direction, channel, agc)
    }

    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        HydraSdr::agc_enabled(self, direction, channel)
    }
}

impl GainControl for HydraSdr {
    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        HydraSdr::gain_elements(self, direction, channel)
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        HydraSdr::set_gain(self, direction, channel, gain)
    }

    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        HydraSdr::gain(self, direction, channel)
    }

    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        HydraSdr::gain_range(self, direction, channel)
    }

    fn set_gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        HydraSdr::set_gain_element(self, direction, channel, name, gain)
    }

    fn gain_element(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        HydraSdr::gain_element(self, direction, channel, name)
    }

    fn gain_element_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        HydraSdr::gain_element_range(self, direction, channel, name)
    }
}

impl FrequencyControl for HydraSdr {
    fn frequency_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        HydraSdr::frequency_range(self, direction, channel)
    }

    fn frequency(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        HydraSdr::frequency(self, direction, channel)
    }

    fn set_frequency(
        &self,
        direction: Direction,
        channel: usize,
        frequency: f64,
        args: Args,
    ) -> Result<(), Error> {
        HydraSdr::set_frequency(self, direction, channel, frequency, args)
    }

    fn frequency_components(
        &self,
        direction: Direction,
        channel: usize,
    ) -> Result<Vec<String>, Error> {
        HydraSdr::frequency_components(self, direction, channel)
    }

    fn component_frequency_range(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        HydraSdr::component_frequency_range(self, direction, channel, name)
    }

    fn component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<f64, Error> {
        HydraSdr::component_frequency(self, direction, channel, name)
    }

    fn set_component_frequency(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
        frequency: f64,
    ) -> Result<(), Error> {
        HydraSdr::set_component_frequency(self, direction, channel, name, frequency)
    }
}

impl SampleRateControl for HydraSdr {
    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        HydraSdr::sample_rate(self, direction, channel)
    }

    fn set_sample_rate(
        &self,
        direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        HydraSdr::set_sample_rate(self, direction, channel, rate)
    }

    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        HydraSdr::get_sample_rate_range(self, direction, channel)
    }
}

impl BandwidthControl for HydraSdr {
    fn bandwidth(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        HydraSdr::bandwidth(self, direction, channel)
    }

    fn set_bandwidth(&self, direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        HydraSdr::set_bandwidth(self, direction, channel, bw)
    }

    fn get_bandwidth_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        HydraSdr::get_bandwidth_range(self, direction, channel)
    }
}

impl RxStreamer {
    fn new(session: Arc<Mutex<SyncHydraSession>>, rx_stream_active: Arc<AtomicBool>) -> Self {
        Self {
            session,
            rx_stream_active,
            iq_scratch: Vec::new(),
            active: false,
        }
    }
}

impl crate::RxStreamer for RxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        Ok(F32_RX_MTU)
    }

    fn activate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedActivation));
        }
        if self.active {
            return Ok(());
        }
        let claim = SyncActivationClaim::acquire(&self.rx_stream_active)?;
        let mut session = self.session.lock().unwrap();
        session
            .ensure_stream()?
            .start()
            .map_err(map_hydrasdr_error)?;
        self.active = true;
        claim.commit();
        Ok(())
    }

    fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedDeactivation));
        }
        if self.active {
            self.session.lock().unwrap().stop_stream()?;
            self.active = false;
            self.rx_stream_active.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    fn read(&mut self, buffers: &mut [&mut [Complex32]], timeout_us: i64) -> Result<usize, Error> {
        if !self.active {
            return Err(Error::StreamInactive);
        }
        crate::streamer::expect_buffer_count(buffers.len(), 1)?;
        if buffers[0].is_empty() {
            return Ok(0);
        }

        let out = &mut buffers[0];
        let read_len = out.len().min(F32_RX_MTU);
        let timeout = if timeout_us < 0 {
            Duration::MAX
        } else {
            Duration::from_micros(timeout_us as u64)
        };
        self.iq_scratch.resize(read_len, (0.0, 0.0));
        let mut session = self.session.lock().unwrap();
        let read = session
            .ensure_stream()?
            .read(&mut self.iq_scratch, timeout)
            .map_err(map_hydrasdr_error)?;
        for (dst, (i, q)) in out
            .iter_mut()
            .take(read)
            .zip(self.iq_scratch.iter().copied())
        {
            *dst = Complex32::new(i, q);
        }
        Ok(read)
    }
}

impl Drop for RxStreamer {
    fn drop(&mut self) {
        let _ = <Self as crate::RxStreamer>::deactivate_at(self, None);
        if self.active {
            self.rx_stream_active.store(false, Ordering::SeqCst);
            self.active = false;
        }
    }
}

impl crate::TxStreamer for TxDummy {
    fn mtu(&self) -> Result<usize, Error> {
        unreachable!()
    }

    fn activate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        unreachable!()
    }

    fn deactivate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        unreachable!()
    }

    fn write(
        &mut self,
        _buffers: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _timeout_us: i64,
    ) -> Result<usize, Error> {
        unreachable!()
    }

    fn write_all(
        &mut self,
        _buffers: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _timeout_us: i64,
    ) -> Result<(), Error> {
        unreachable!()
    }
}

fn open_selected_device(selector: DeviceSelector) -> Result<(HydraSdrDevice, Option<u64>), Error> {
    match selector {
        DeviceSelector::First => HydraSdrDevice::builder()
            .sample_format(SampleFormat::F32Iq)
            .decimation_mode(DecimationMode::HighDefinition)
            .open()
            .wait()
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
            .wait()
            .map(|dev| (dev, Some(serial)))
            .map_err(map_hydrasdr_error),
        DeviceSelector::Index(index) => {
            let devices = HydraSdrDevice::list().wait().map_err(map_hydrasdr_error)?;
            let Some(info) = devices.get(index) else {
                return Err(Error::DeviceNotFound);
            };
            if let Some(serial) = info.serial {
                HydraSdrDevice::builder()
                    .serial(serial)
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open()
                    .wait()
                    .map(|dev| (dev, Some(serial)))
                    .map_err(map_hydrasdr_error)
            } else if index == 0 {
                HydraSdrDevice::builder()
                    .sample_format(SampleFormat::F32Iq)
                    .decimation_mode(DecimationMode::HighDefinition)
                    .open()
                    .wait()
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

    #[test]
    fn synchronous_activation_claim_is_exclusive() {
        let active = Arc::new(AtomicBool::new(false));
        let claim = SyncActivationClaim::acquire(&active).expect("acquire stream claim");

        assert!(active.load(Ordering::SeqCst));
        assert!(matches!(
            SyncActivationClaim::acquire(&active),
            Err(Error::Busy)
        ));

        claim.commit();
        assert!(active.load(Ordering::SeqCst));
    }

    #[test]
    fn abandoned_synchronous_activation_releases_claim() {
        let active = Arc::new(AtomicBool::new(false));
        let claim = SyncActivationClaim::acquire(&active).expect("acquire stream claim");
        drop(claim);

        assert!(!active.load(Ordering::SeqCst));
    }
}
