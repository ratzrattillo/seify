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
    Args, AsyncAgcControl, AsyncAntennaControl, AsyncDeviceInfo, AsyncFrequencyControl,
    AsyncGainControl, AsyncRxDevice, AsyncSampleRateControl, Capability, Direction, Driver, Error,
    Range, RangeItem,
};

/// Asynchronous HydraSDR RFOne device backend.
#[derive(Clone)]
pub struct AsyncHydraSdr {
    session_slot: Shared<AsyncSessionSlot>,
    serial: Option<u64>,
    inner: Shared<AsyncMutex<ReceiverContext>>,
    streamer_claimed: Shared<AtomicBool>,
    cleanup_needed: Shared<AtomicBool>,
}

/// HydraSDR RFOne asynchronous receive streamer.
///
/// The streamer exclusively owns the HydraSDR session until it is deactivated or dropped.
/// Explicit deactivation stops reception and returns the session to the device so that settings
/// can be changed before reactivation. Dropping an active streamer leaves receiver-off cleanup to
/// the next asynchronous device or stream operation.
#[must_use = "deactivate the HydraSDR stream before dropping it"]
pub struct AsyncHydraSdrRxStreamer {
    session_slot: Shared<AsyncSessionSlot>,
    streamer_claimed: Shared<AtomicBool>,
    cleanup_needed: Shared<AtomicBool>,
    session: Option<AsyncHydraSession>,
    active: bool,
    stop_required: bool,
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

    async fn stop_stream(&mut self) -> Result<(), Error> {
        match self {
            Self::Device(_) => Ok(()),
            Self::Stream(stream) => stream.stop().await.map(|_| ()).map_err(map_hydrasdr_error),
            Self::Disconnected => Err(Error::DeviceDisconnected),
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

#[cfg(not(target_arch = "wasm32"))]
struct AsyncSessionSlot(std::sync::Mutex<Option<AsyncHydraSession>>);

#[cfg(target_arch = "wasm32")]
struct AsyncSessionSlot(std::cell::RefCell<Option<AsyncHydraSession>>);

impl AsyncSessionSlot {
    fn new(session: AsyncHydraSession) -> Self {
        Self(Default::default()).with_session(session)
    }

    fn with_session(self, session: AsyncHydraSession) -> Self {
        let result = self.put(session);
        debug_assert!(result.is_ok());
        self
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn take(&self) -> Option<AsyncHydraSession> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    #[cfg(target_arch = "wasm32")]
    fn take(&self) -> Option<AsyncHydraSession> {
        self.0.borrow_mut().take()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn put(&self, session: AsyncHydraSession) -> Result<(), AsyncHydraSession> {
        let mut slot = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_some() {
            Err(session)
        } else {
            *slot = Some(session);
            Ok(())
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn put(&self, session: AsyncHydraSession) -> Result<(), AsyncHydraSession> {
        let mut slot = self.0.borrow_mut();
        if slot.is_some() {
            Err(session)
        } else {
            *slot = Some(session);
            Ok(())
        }
    }
}

struct AsyncSessionLease {
    slot: Shared<AsyncSessionSlot>,
    session: Option<AsyncHydraSession>,
}

impl AsyncSessionLease {
    fn acquire(slot: &Shared<AsyncSessionSlot>) -> Result<Self, Error> {
        let session = slot.take().ok_or(Error::Busy)?;
        Ok(Self {
            slot: Shared::clone(slot),
            session: Some(session),
        })
    }

    fn session_mut(&mut self) -> &mut AsyncHydraSession {
        self.session
            .as_mut()
            .expect("session lease always owns a session")
    }

    fn into_session(mut self) -> AsyncHydraSession {
        self.session
            .take()
            .expect("session lease always owns a session")
    }
}

impl Drop for AsyncSessionLease {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let result = self.slot.put(session);
            debug_assert!(result.is_ok(), "session slot was unexpectedly occupied");
        }
    }
}

struct AsyncStreamerClaim {
    streamer_claimed: Shared<AtomicBool>,
    committed: bool,
}

impl AsyncStreamerClaim {
    fn acquire(streamer_claimed: &Shared<AtomicBool>) -> Result<Self, Error> {
        streamer_claimed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| Error::Busy)?;
        Ok(Self {
            streamer_claimed: Shared::clone(streamer_claimed),
            committed: false,
        })
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for AsyncStreamerClaim {
    fn drop(&mut self) {
        if !self.committed {
            self.streamer_claimed.store(false, Ordering::SeqCst);
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
    session.stop_stream().await?;
    cleanup_needed.store(false, Ordering::SeqCst);
    Ok(())
}

impl AsyncHydraSdr {
    /// Return descriptors for detected HydraSDR RFOne devices asynchronously.
    pub async fn probe(args: &Args) -> Result<Vec<Args>, Error> {
        let devices = HydraSdrDevice::list().await.map_err(map_hydrasdr_error)?;
        probe_args(args, devices)
    }

    /// Open a HydraSDR RFOne device from arguments asynchronously.
    pub async fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let selector = device_selector(&args)?;
        let (mut dev, serial) = open_selected_device_async(selector).await?;
        let sample_rates = dev.sample_rates().await.map_err(map_hydrasdr_error)?;
        let receiver_context = ReceiverContext::from_device_info(dev.info(), sample_rates);

        Ok(Self {
            session_slot: Shared::new(AsyncSessionSlot::new(AsyncHydraSession::Device(Box::new(
                dev,
            )))),
            serial,
            inner: Shared::new(AsyncMutex::new(receiver_context)),
            streamer_claimed: Shared::new(AtomicBool::new(false)),
            cleanup_needed: Shared::new(AtomicBool::new(false)),
        })
    }

    async fn lease_idle_session(&self) -> Result<AsyncSessionLease, Error> {
        let mut session = AsyncSessionLease::acquire(&self.session_slot)?;
        cleanup_abandoned_session(session.session_mut(), &self.cleanup_needed).await?;
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
        Ok(self.inner.lock().await.antennas())
    }

    async fn antenna(&self, direction: Direction, channel: usize) -> Result<String, Error> {
        check_rx(direction, channel)?;
        self.inner.lock().await.antenna()
    }

    async fn set_antenna(
        &self,
        direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<(), Error> {
        check_rx(direction, channel)?;
        let port =
            self.inner
                .lock()
                .await
                .rf_port_for_antenna(name)
                .ok_or(Error::invalid_argument(
                    "antenna",
                    "antenna is not available on this HydraSDR device",
                ))?;
        let mut session = self.lease_idle_session().await?;
        session.session_mut().set_rf_port(port).await
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
        let mut session = self.lease_idle_session().await?;
        session.session_mut().set_gain(agc_gain_config(agc)).await
    }

    async fn agc_enabled(&self, direction: Direction, channel: usize) -> Result<bool, Error> {
        check_rx(direction, channel)?;
        self.inner.lock().await.agc_enabled()
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
        check_rx(direction, channel)?;
        let range = overall_gain_range();
        if !range.contains(gain) {
            return Err(Error::out_of_range("gain", range, gain));
        }

        let mut session = self.lease_idle_session().await?;
        for (gain_type, value) in distribute_overall_gain(gain) {
            session
                .session_mut()
                .set_gain(gain_type.update(value))
                .await?;
        }
        Ok(())
    }

    async fn gain(&self, direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        check_rx(direction, channel)?;
        self.inner.lock().await.overall_gain()
    }

    async fn gain_range(&self, direction: Direction, channel: usize) -> Result<Range, Error> {
        check_rx(direction, channel)?;
        Ok(overall_gain_range())
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
        let mut session = self.lease_idle_session().await?;
        session.session_mut().set_gain(gain_update).await
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
        self.inner.lock().await.gain_value(gain_type)
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
        self.inner.lock().await.frequency()
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
        let mut session = self.lease_idle_session().await?;
        session
            .session_mut()
            .set_frequency_hz(frequency as u64)
            .await
    }

    async fn sample_rate(&self, direction: Direction, channel: usize) -> Result<f64, Error> {
        check_rx(direction, channel)?;
        self.inner.lock().await.sample_rate()
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
        let mut session = self.lease_idle_session().await?;
        session.session_mut().set_sample_rate_hz(rate as u32).await
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
    AsyncHydraSdr => [rx, antenna, agc, gain, frequency, sample_rate]
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
        let claim = AsyncStreamerClaim::acquire(&self.streamer_claimed)?;
        let session = self.lease_idle_session().await?.into_session();
        let streamer = AsyncHydraSdrRxStreamer::new(
            Shared::clone(&self.session_slot),
            Shared::clone(&self.streamer_claimed),
            Shared::clone(&self.cleanup_needed),
            session,
        );
        claim.commit();
        Ok(streamer)
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

impl AsyncHydraSdrRxStreamer {
    fn new(
        session_slot: Shared<AsyncSessionSlot>,
        streamer_claimed: Shared<AtomicBool>,
        cleanup_needed: Shared<AtomicBool>,
        session: AsyncHydraSession,
    ) -> Self {
        Self {
            session_slot,
            streamer_claimed,
            cleanup_needed,
            session: Some(session),
            active: false,
            stop_required: false,
        }
    }

    async fn take_session(&mut self) -> Result<(), Error> {
        if self.session.is_none() {
            let mut session = AsyncSessionLease::acquire(&self.session_slot)?;
            cleanup_abandoned_session(session.session_mut(), &self.cleanup_needed).await?;
            self.session = Some(session.into_session());
        } else if self.cleanup_needed.load(Ordering::SeqCst) {
            cleanup_abandoned_session(
                self.session.as_mut().ok_or(Error::DeviceDisconnected)?,
                &self.cleanup_needed,
            )
            .await?;
            self.stop_required = false;
            self.active = false;
        }
        Ok(())
    }

    fn return_session(&mut self) -> Result<(), Error> {
        let Some(session) = self.session.take() else {
            return Ok(());
        };
        match self.session_slot.put(session) {
            Ok(()) => Ok(()),
            Err(session) => {
                self.session = Some(session);
                Err(Error::Busy)
            }
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
        self.take_session().await?;
        let session = self.session.as_mut().ok_or(Error::DeviceDisconnected)?;
        self.cleanup_needed.store(true, Ordering::SeqCst);
        self.stop_required = true;
        session
            .ensure_stream()?
            .start()
            .await
            .map_err(map_hydrasdr_error)?;
        self.cleanup_needed.store(false, Ordering::SeqCst);
        self.active = true;
        Ok(())
    }

    async fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if time_ns.is_some() {
            return Err(Error::unsupported(Capability::TimedDeactivation));
        }
        if self.stop_required {
            self.cleanup_needed.store(true, Ordering::SeqCst);
            self.active = false;
            self.session
                .as_mut()
                .ok_or(Error::DeviceDisconnected)?
                .stop_stream()
                .await?;
            self.stop_required = false;
            self.cleanup_needed.store(false, Ordering::SeqCst);
        }
        self.active = false;
        self.return_session()
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
        let Some(AsyncHydraSession::Stream(stream)) = self.session.as_mut() else {
            return Err(Error::DeviceDisconnected);
        };
        let read = match with_timeout(stream.read(out), timeout_from_micros(timeout_us)).await {
            TimeoutResult::Completed(read) => read.map_err(map_hydrasdr_error)?,
            TimeoutResult::TimedOut => 0,
        };
        Ok(read)
    }
}

impl Drop for AsyncHydraSdrRxStreamer {
    fn drop(&mut self) {
        if self.stop_required {
            self.cleanup_needed.store(true, Ordering::SeqCst);
        }
        if let Some(session) = self.session.take() {
            let result = self.session_slot.put(session);
            debug_assert!(result.is_ok(), "session slot was unexpectedly occupied");
        }
        self.active = false;
        self.streamer_claimed.store(false, Ordering::SeqCst);
    }
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
    fn abandoned_async_streamer_creation_releases_claim() {
        let claimed = Shared::new(AtomicBool::new(false));

        let claim = AsyncStreamerClaim::acquire(&claimed).expect("acquire streamer claim");
        assert!(claimed.load(Ordering::SeqCst));
        drop(claim);

        assert!(!claimed.load(Ordering::SeqCst));
    }

    #[cfg(any(feature = "smol", feature = "tokio"))]
    #[test]
    fn committed_async_streamer_claim_is_exclusive() {
        let claimed = Shared::new(AtomicBool::new(false));

        let claim = AsyncStreamerClaim::acquire(&claimed).expect("acquire streamer claim");
        claim.commit();

        assert!(claimed.load(Ordering::SeqCst));
        assert!(matches!(
            AsyncStreamerClaim::acquire(&claimed),
            Err(Error::Busy)
        ));
    }

    #[cfg(any(feature = "smol", feature = "tokio"))]
    #[test]
    fn dropping_async_session_lease_returns_session() {
        let slot = Shared::new(AsyncSessionSlot::new(AsyncHydraSession::Disconnected));

        let lease = AsyncSessionLease::acquire(&slot).expect("acquire session lease");
        assert!(matches!(
            AsyncSessionLease::acquire(&slot),
            Err(Error::Busy)
        ));
        drop(lease);

        assert!(AsyncSessionLease::acquire(&slot).is_ok());
    }
}
