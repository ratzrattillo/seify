use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hydrasdr_rs::{
    DecimationMode, Device as HydraSdrDevice, F32RxStream, GainConfig, MaybeFuture, RfPort,
    SampleFormat,
};
use num_complex::Complex32;

use super::common::*;
use crate::Direction::*;
use crate::{
    AgcControl, AntennaControl, Args, Capability, DeviceInfo, Direction, Driver, Error,
    FrequencyControl, GainControl, Range, RangeItem, RxDevice, SampleRateControl,
};

/// HydraSDR RFOne device backend.
#[derive(Clone)]
pub struct HydraSdr {
    session_slot: Arc<Mutex<Option<SyncHydraSession>>>,
    serial: Option<u64>,
    inner: Arc<ReceiverContext>,
    streamer_claimed: Arc<AtomicBool>,
}
/// Exclusively claimed HydraSDR RFOne receive streamer.
///
/// The streamer owns the driver session while receiving and returns it to the
/// device on deactivation so focused configuration remains available.
pub struct RxStreamer {
    session_slot: Arc<Mutex<Option<SyncHydraSession>>>,
    streamer_claimed: Arc<AtomicBool>,
    session: Option<SyncHydraSession>,
    active: bool,
    stop_required: bool,
}

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

struct SyncStreamerClaim {
    streamer_claimed: Arc<AtomicBool>,
    committed: bool,
}

impl SyncStreamerClaim {
    fn acquire(streamer_claimed: &Arc<AtomicBool>) -> Result<Self, Error> {
        streamer_claimed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            streamer_claimed: Arc::clone(streamer_claimed),
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for SyncStreamerClaim {
    fn drop(&mut self) {
        if !self.committed {
            self.streamer_claimed.store(false, Ordering::SeqCst);
        }
    }
}

impl HydraSdr {
    /// Return descriptors for detected HydraSDR RFOne devices.
    pub fn probe(args: &Args) -> Result<Vec<Args>, Error> {
        let devices = HydraSdrDevice::list().wait().map_err(map_hydrasdr_error)?;
        probe_args(args, devices)
    }

    /// Open a HydraSDR RFOne device from arguments.
    pub fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let selector = device_selector(&args)?;
        let (dev, serial) = open_selected_device(selector)?;
        let sample_rates = dev.sample_rates();
        let receiver_context = ReceiverContext::from_device_info(dev.info(), sample_rates);

        Ok(Self {
            session_slot: Arc::new(Mutex::new(Some(SyncHydraSession::Device(Box::new(dev))))),
            serial,
            inner: Arc::new(receiver_context),
            streamer_claimed: Arc::new(AtomicBool::new(false)),
        })
    }

    fn with_idle_session<T>(
        &self,
        operation: impl FnOnce(&mut SyncHydraSession) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let mut slot = self.session_slot.lock().unwrap();
        let session = slot.as_mut().ok_or(Error::Busy)?;
        session.stop_stream()?;
        operation(session)
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
        Ok(self.inner.antennas())
    }

    fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        check_rx(direction, channel)?;
        self.inner.antenna()
    }

    fn set_antenna(&self, direction: Direction, channel: usize, name: &str) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let port = self
            .inner
            .rf_port_for_antenna(name)
            .ok_or(Error::invalid_argument(
                "antenna",
                "antenna is not available on this HydraSDR device",
            ))?;
        self.with_idle_session(|session| session.set_rf_port(port))
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
        self.with_idle_session(|session| session.set_gain(agc_gain_config(agc)))
    }

    fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        self.inner.agc_enabled()
    }

    fn gain_elements(&self, direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        check_rx(direction, channel)?;
        Ok(self
            .inner
            .gains
            .iter()
            .map(|gain| gain.name.to_string())
            .collect())
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let range = overall_gain_range();
        if !range.contains(gain) {
            return Err(Error::out_of_range("gain", range, gain));
        }

        self.with_idle_session(|session| {
            for (gain_type, value) in distribute_overall_gain(gain) {
                session.set_gain(gain_type.update(value))?;
            }
            Ok(())
        })
    }

    fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        check_rx(direction, channel)?;
        self.inner.overall_gain()
    }

    fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        Ok(overall_gain_range())
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

        let gain_update = gain_type.update(gain);
        self.with_idle_session(|session| session.set_gain(gain_update))
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
        self.inner.gain_value(gain_type)
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
        self.inner
            .gain_range(gain_type)
            .ok_or(Error::invalid_argument(
                "hydrasdr",
                "invalid HydraSDR argument",
            ))
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
            Ok(Range::new(vec![RangeItem::Interval(
                self.inner.min_frequency,
                self.inner.max_frequency,
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
        self.inner.frequency()
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
        self.with_idle_session(|session| session.set_frequency_hz(frequency as u64))
    }

    fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner.sample_rate()
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
        self.with_idle_session(|session| session.set_sample_rate_hz(rate as u32))
    }

    fn get_sample_rate_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        sample_rate_range(&self.inner.sample_rates)
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
    HydraSdr => [rx, antenna, agc, gain, frequency, sample_rate]
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
        let claim = SyncStreamerClaim::acquire(&self.streamer_claimed)?;
        let mut session = self
            .session_slot
            .lock()
            .unwrap()
            .take()
            .ok_or(Error::Busy)?;
        if let Err(error) = session.stop_stream() {
            *self.session_slot.lock().unwrap() = Some(session);
            return Err(error);
        }
        let streamer = RxStreamer::new(
            Arc::clone(&self.session_slot),
            Arc::clone(&self.streamer_claimed),
            session,
        );
        claim.commit();
        Ok(streamer)
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

impl RxStreamer {
    fn new(
        session_slot: Arc<Mutex<Option<SyncHydraSession>>>,
        streamer_claimed: Arc<AtomicBool>,
        session: SyncHydraSession,
    ) -> Self {
        Self {
            session_slot,
            streamer_claimed,
            session: Some(session),
            active: false,
            stop_required: false,
        }
    }

    fn take_session(&mut self) -> Result<(), Error> {
        if self.session.is_none() {
            self.session = self.session_slot.lock().unwrap().take();
        }
        if self.session.is_some() {
            Ok(())
        } else {
            Err(Error::Busy)
        }
    }

    fn return_session(&mut self) -> Result<(), Error> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        let mut slot = self.session_slot.lock().unwrap();
        if slot.is_some() {
            self.session = Some(session);
            return Err(Error::Busy);
        }
        *slot = Some(session);
        Ok(())
    }

    fn stop_and_return_session(&mut self) -> Result<(), Error> {
        if self.stop_required {
            self.session
                .as_mut()
                .ok_or(Error::DeviceDisconnected)?
                .stop_stream()?;
            self.stop_required = false;
        }
        self.active = false;
        self.return_session()
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
        self.take_session()?;
        self.stop_required = true;
        self.session
            .as_mut()
            .ok_or(Error::DeviceDisconnected)?
            .ensure_stream()?
            .start()
            .map_err(map_hydrasdr_error)?;
        self.active = true;
        Ok(())
    }

    fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedDeactivation));
        }
        self.stop_and_return_session()
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
        self.session
            .as_mut()
            .ok_or(Error::DeviceDisconnected)?
            .ensure_stream()?
            .read(&mut out[..read_len], timeout)
            .map_err(map_hydrasdr_error)
    }
}

impl Drop for RxStreamer {
    fn drop(&mut self) {
        if self.stop_required {
            if let Some(session) = self.session.as_mut() {
                let _ = session.stop_stream();
            }
            self.stop_required = false;
        }
        self.active = false;
        let _ = self.return_session();
        self.streamer_claimed.store(false, Ordering::SeqCst);
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
    fn synchronous_streamer_claim_is_exclusive() {
        let claimed = Arc::new(AtomicBool::new(false));
        let claim = SyncStreamerClaim::acquire(&claimed).expect("acquire streamer claim");

        assert!(claimed.load(Ordering::SeqCst));
        assert!(matches!(
            SyncStreamerClaim::acquire(&claimed),
            Err(Error::Busy)
        ));

        claim.commit();
        assert!(claimed.load(Ordering::SeqCst));
    }

    #[test]
    fn abandoned_synchronous_streamer_creation_releases_claim() {
        let claimed = Arc::new(AtomicBool::new(false));
        let claim = SyncStreamerClaim::acquire(&claimed).expect("acquire streamer claim");
        drop(claim);

        assert!(!claimed.load(Ordering::SeqCst));
    }
}
