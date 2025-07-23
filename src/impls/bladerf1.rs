use crate::{Args, Direction, Error, Range, RangeItem};
use libbladerf_rs::bladerf1::BladerfXb::BladerfXb200;
use libbladerf_rs::bladerf1::{BladerfXb, BLADERF_FREQUENCY_MIN};
use libbladerf_rs::board::bladerf1::BladeRf1;
use libbladerf_rs::BladerfGainMode;
use libbladerf_rs::{BladeRf1RxStreamer, BladeRf1TxStreamer};
use num_complex::Complex32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::thread::sleep;
use std::time::Duration;

pub struct BladeRf {
    inner: BladeRf1,
}

impl BladeRf {
    pub fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        let dev_infos = BladeRf1::list_bladerf1()
            .map_err(|_| Error::NotFound)?
            .collect::<Vec<_>>();

        log::trace!("dev_infos: {dev_infos:?}");
        let mut devs = vec![];
        for dev in dev_infos {
            devs.push(
                format!(
                    "driver=bladerf, bus_number={}, address={}",
                    dev.busnum(),
                    dev.device_address()
                )
                .try_into()?,
            );
        }
        Ok(devs)
    }

    /// Create a BladeRf1 devices
    pub fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args: Args = args.try_into().or(Err(Error::ValueError))?;

        log::trace!("args: {args:?}");
        if let Ok(fd) = args.get::<i32>("fd") {
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let bladerf = BladeRf1::from_fd(fd).map_err(|e| Error::Misc(e.to_string()))?;
            bladerf
                .initialize()
                .map_err(|e| Error::Misc(e.to_string()))?;
            return Ok(Self { inner: bladerf });
        }

        let bus_number = args.get("bus_number");
        let address = args.get("address");
        let dev = match (bus_number, address) {
            (Ok(bus_number), Ok(address)) => {
                let bladerf = BladeRf1::from_bus_addr(bus_number, address)
                    .map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
                    .initialize()
                    .map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
            }
            (Err(Error::NotFound), Err(Error::NotFound)) => {
                log::trace!("Opening first bladerf device");
                let bladerf = BladeRf1::from_first().map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
                    .initialize()
                    .map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
            }
            (bus_number, address) => {
                log::error!("BladeRf::open received invalid args: bus_number: {bus_number:?}, address: {address:?}");
                return Err(Error::ValueError);
            }
        };

        Ok(Self { inner: dev })
    }

    pub fn enable_expansion_board(&mut self, board_type: BladerfXb) -> Result<(), Error> {
        self.inner
            .expansion_attach(board_type)
            .map_err(|e| Error::Misc(e.to_string()))
    }
}

pub struct RxStreamer {
    streamer: BladeRf1RxStreamer,
}

pub struct TxStreamer {
    streamer: BladeRf1TxStreamer,
}

impl crate::RxStreamer for RxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        self.streamer.mtu().map_err(|e| Error::Misc(e.to_string()))
    }

    fn activate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if let Some(t) = time_ns {
            sleep(Duration::from_nanos(t as u64));
        }
        self.streamer
            .activate()
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if let Some(t) = time_ns {
            sleep(Duration::from_nanos(t as u64));
        }
        self.streamer
            .deactivate()
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn read(&mut self, buffers: &mut [&mut [Complex32]], timeout_us: i64) -> Result<usize, Error> {
        self.streamer
            .read_sync(buffers, timeout_us)
            .map_err(|e| Error::Misc(e.to_string()))
    }
}

impl crate::TxStreamer for TxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        self.streamer.mtu().map_err(|e| Error::Misc(e.to_string()))
    }

    fn activate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if let Some(t) = time_ns {
            sleep(Duration::from_nanos(t as u64));
        }
        self.streamer
            .activate()
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn deactivate_at(&mut self, time_ns: Option<i64>) -> Result<(), Error> {
        if let Some(t) = time_ns {
            sleep(Duration::from_nanos(t as u64));
        }
        self.streamer
            .deactivate()
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn write(
        &mut self,
        _buffers: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _timeout_us: i64,
    ) -> Result<usize, Error> {
        Err(Error::NotSupported)
    }

    fn write_all(
        &mut self,
        buffers: &[&[Complex32]],
        at_ns: Option<i64>,
        end_burst: bool,
        timeout_us: i64,
    ) -> Result<(), Error> {
        self.streamer
            .write_all(buffers, at_ns, end_burst, timeout_us)
            .map_err(|e| Error::Misc(e.to_string()))
    }
}

impl crate::DeviceTrait for BladeRf {
    type RxStreamer = RxStreamer;

    type TxStreamer = TxStreamer;

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn driver(&self) -> crate::Driver {
        crate::Driver::BladeRf
    }

    fn id(&self) -> Result<String, Error> {
        self.inner.serial().map_err(|e| Error::Misc(e.to_string()))
    }

    fn info(&self) -> Result<Args, Error> {
        let mut args = Args::default();
        let fw_version = self
            .inner
            .fx3_firmware()
            .map_err(|e| Error::Misc(e.to_string()))?;
        args.set("firmware version", fw_version);
        Ok(args)
    }

    fn num_channels(&self, _: Direction) -> Result<usize, Error> {
        Ok(1)
    }

    fn full_duplex(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Ok(true)
    }

    fn rx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::RxStreamer, Error> {
        if channels != [0] {
            log::error!("BladeRF1 only supports one RX channel!");
            Err(Error::ValueError)
        } else {
            // TODO: Find a way not to have to call clone on self.inner
            let streamer = BladeRf1RxStreamer::new(self.inner.clone(), 65536, Some(8), None)
                .map_err(|e| Error::Misc(e.to_string()))?;
            Ok(RxStreamer { streamer })
        }
    }

    fn tx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::TxStreamer, Error> {
        if channels != [0] {
            log::error!("BladeRF1 only supports one TX channel!");
            Err(Error::ValueError)
        } else {
            // TODO: Find a way not to have to call clone on self.inner
            let streamer = BladeRf1TxStreamer::new(self.inner.clone(), 65536, Some(8), None)
                .map_err(|e| Error::Misc(e.to_string()))?;
            Ok(TxStreamer { streamer })
        }
    }

    fn antennas(&self, _direction: Direction, _channel: usize) -> Result<Vec<String>, Error> {
        Err(Error::NotSupported)
    }

    fn antenna(&self, _direction: Direction, _channel: usize) -> Result<String, Error> {
        Err(Error::NotSupported)
    }

    fn set_antenna(
        &self,
        _direction: Direction,
        _channel: usize,
        _name: &str,
    ) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn supports_agc(&self, _direction: Direction, channel: usize) -> Result<bool, Error> {
        Ok(self.inner.get_gain_modes(channel as u8).is_ok())
    }

    fn enable_agc(&self, _direction: Direction, channel: usize, agc: bool) -> Result<(), Error> {
        let gain_mode = if agc {
            BladerfGainMode::Default
        } else {
            BladerfGainMode::Mgc
        };

        self.inner
            .set_gain_mode(channel as u8, gain_mode)
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn agc(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Ok(self.inner.get_gain_mode().is_ok())
    }

    fn gain_elements(&self, _direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        Ok(BladeRf1::get_gain_stages(channel as u8))
    }

    fn set_gain(&self, _direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.inner
            .set_gain(channel as u8, gain as i8)
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn gain(&self, _direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        Ok(Some(
            self.inner
                .get_gain(channel as u8)
                .map_err(|e| Error::Misc(e.to_string()))? as f64,
        ))
    }

    fn gain_range(&self, _direction: Direction, channel: usize) -> Result<Range, Error> {
        let range = BladeRf1::get_gain_range(channel as u8);
        let ri = RangeItem::Step(range.min as f64, range.max as f64, range.step as f64);
        Ok(Range { items: vec![ri] })
    }

    fn set_gain_element(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
        gain: f64,
    ) -> Result<(), Error> {
        self.inner
            .set_gain_stage(channel as u8, name, gain as i8)
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn gain_element(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        Ok(Some(
            self.inner
                .get_gain_stage(channel as u8, name)
                .map_err(|e| Error::Misc(e.to_string()))? as f64,
        ))
    }

    fn gain_element_range(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        // TODO: add support for other gains
        let range = BladeRf1::get_gain_stage_range(channel as u8, name)
            .map_err(|e| Error::Misc(e.to_string()))?;
        Ok(Range {
            items: vec![RangeItem::Step(
                range.min as f64,
                range.max as f64,
                range.step as f64,
            )],
        })
    }

    fn frequency_range(&self, _direction: Direction, _channel: usize) -> Result<Range, Error> {
        let bladerf1_range = self
            .inner
            .get_frequency_range()
            .map_err(|_| Error::ValueError)?;
        let min_freq = bladerf1_range.min as f64;
        let max_freq = bladerf1_range.max as f64;
        let seify_range = RangeItem::Step(min_freq, max_freq, 1f64);
        Ok(Range::new(vec![seify_range]))
    }

    fn frequency(&self, _direction: Direction, channel: usize) -> Result<f64, Error> {
        Ok(self
            .inner
            .get_frequency(channel as u8)
            .map_err(|e| Error::Misc(e.to_string()))? as f64)
    }

    fn set_frequency(
        &self,
        _direction: Direction,
        channel: usize,
        frequency: f64,
        _args: Args,
    ) -> Result<(), Error> {
        if frequency < BLADERF_FREQUENCY_MIN as f64 {
            log::trace!("Frequency {frequency} requires XB200 expansion board");
            let xb = self
                .inner
                .expansion_get_attached()
                .map_err(|_| Error::ValueError)?;
            if xb != BladerfXb200 {
                log::debug!("Automatically attaching XB200 expansion board");
                self.inner
                    .expansion_attach(BladerfXb200)
                    .map_err(|e| Error::Misc(e.to_string()))?;
            }
        }
        log::trace!("Setting frequency to {frequency}");

        self.inner
            .set_frequency(channel as u8, frequency as u64)
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn frequency_components(
        &self,
        _direction: Direction,
        _channel: usize,
    ) -> Result<Vec<String>, Error> {
        Err(Error::ValueError)
    }

    fn component_frequency_range(
        &self,
        _direction: Direction,
        _channel: usize,
        _name: &str,
    ) -> Result<Range, Error> {
        Err(Error::ValueError)
    }

    fn component_frequency(
        &self,
        _direction: Direction,
        _channel: usize,
        _name: &str,
    ) -> Result<f64, Error> {
        Err(Error::ValueError)
    }

    fn set_component_frequency(
        &self,
        _direction: Direction,
        _channel: usize,
        _name: &str,
        _frequency: f64,
    ) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn sample_rate(&self, _direction: Direction, channel: usize) -> Result<f64, Error> {
        Ok(self
            .inner
            .get_sample_rate(channel as u8)
            .map_err(|e| Error::Misc(e.to_string()))? as f64)
    }

    fn set_sample_rate(
        &self,
        _direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        self.inner
            .set_sample_rate(channel.try_into().unwrap(), rate as u32)
            .map_err(|e| Error::Misc(e.to_string()))?;
        Ok(())
    }

    fn get_sample_rate_range(
        &self,
        _direction: Direction,
        _channel: usize,
    ) -> Result<Range, Error> {
        let range = BladeRf1::get_sample_rate_range();
        Ok(Range::new(vec![RangeItem::Step(
            range.min as f64,
            range.max as f64,
            range.step as f64,
        )]))
    }

    fn bandwidth(&self, _direction: Direction, channel: usize) -> Result<f64, Error> {
        Ok(self
            .inner
            .get_bandwidth(channel as u8)
            .map_err(|e| Error::Misc(e.to_string()))? as f64)
    }

    fn set_bandwidth(&self, _direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        self.inner
            .set_bandwidth(channel as u8, bw as u32)
            .map_err(|e| Error::Misc(e.to_string()))
    }

    fn get_bandwidth_range(&self, _direction: Direction, _channel: usize) -> Result<Range, Error> {
        let range = BladeRf1::get_bandwidth_range();
        Ok(Range::new(vec![RangeItem::Step(
            range.min as f64,
            range.max as f64,
            range.step as f64,
        )]))
    }

    fn has_dc_offset_mode(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Err(Error::NotSupported)
    }

    fn set_dc_offset_mode(
        &self,
        _direction: Direction,
        _channel: usize,
        _automatic: bool,
    ) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn dc_offset_mode(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Err(Error::NotSupported)
    }
}
