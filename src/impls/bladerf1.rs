use crate::{Args, Direction, Error, Range, RangeItem};
use bladerf_globals::BladerfGainMode;
use futures::executor::block_on;
use libbladerf_rs::board::bladerf1::BladeRf1;
use num_complex::Complex32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

pub struct BladeRf {
    inner: Arc<BladeRf1>,
}

impl BladeRf {
    pub fn probe(_args: &Args) -> Result<Vec<Args>, Error> {
        let dev_infos = block_on(BladeRf1::list_bladerf1())
            .map_err(|_| Error::NotFound)?
            .collect::<Vec<_>>();

        let mut devs = vec![];
        for dev in dev_infos {
            devs.push(
                format!(
                    "driver=hackrfone, bus_number={}, address={}",
                    dev.busnum(),
                    dev.device_address()
                )
                .try_into()?,
            );
        }
        Ok(devs)
    }

    /// Create a BladeRf One devices
    pub fn open<A: TryInto<Args>>(args: A) -> Result<Self, Error> {
        let args: Args = args.try_into().or(Err(Error::ValueError))?;

        if let Ok(fd) = args.get::<i32>("fd") {
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };

            return Ok(Self {
                inner: Arc::new(*block_on(BladeRf1::from_fd(fd)).map_err(|_| Error::ValueError)?),
            });
        }

        let bus_number = args.get("bus_number");
        let address = args.get("address");
        let dev = match (bus_number, address) {
            (Ok(bus_number), Ok(address)) => block_on(BladeRf1::from_bus_addr(bus_number, address)),
            (Err(Error::NotFound), Err(Error::NotFound)) => {
                log::debug!("Opening first bladerf device");
                block_on(BladeRf1::from_first())
            }
            (bus_number, address) => {
                log::warn!("BladeRf::open received invalid args: bus_number: {bus_number:?}, address: {address:?}");
                return Err(Error::ValueError);
            }
        };

        Ok(Self {
            inner: Arc::new(*dev.unwrap()),
        })
    }
}

// TODO Adjust to proper value
const MTU: usize = 4 * 16384;

pub struct RxStreamer {
    dev: Arc<BladeRf1>,
}

pub struct TxStreamer {
    #[allow(dead_code)]
    dev: Arc<BladeRf1>,
}

impl crate::RxStreamer for RxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        Ok(MTU)
    }

    fn activate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn deactivate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn read(&mut self, buffers: &mut [&mut [Complex32]], timeout_us: i64) -> Result<usize, Error> {
        Ok(self
            .dev
            .read_sync(buffers, timeout_us)
            .map_err(|_| Error::ValueError)?)
    }
}

impl crate::TxStreamer for TxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        Ok(MTU)
    }

    fn activate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn deactivate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        Err(Error::NotSupported)
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
        _buffers: &[&[Complex32]],
        _at_ns: Option<i64>,
        _end_burst: bool,
        _timeout_us: i64,
    ) -> Result<(), Error> {
        Err(Error::NotSupported)
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
        Ok(block_on(self.inner.serial()).expect("Could not get device id"))
    }

    fn info(&self) -> Result<Args, Error> {
        let mut args = Args::default();
        args.set(
            "firmware version",
            block_on(self.inner.fx3_firmware()).unwrap_or("unknown firmware".into()),
        );
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
            Err(Error::ValueError)
        } else {
            Ok(RxStreamer {
                dev: self.inner.clone(),
            })
        }
    }

    fn tx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::TxStreamer, Error> {
        if channels != [0] {
            Err(Error::ValueError)
        } else {
            Ok(TxStreamer {
                dev: self.inner.clone(),
            })
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
        Ok(block_on(self.inner.get_gain_modes(channel as u8)).is_ok())
    }

    fn enable_agc(&self, _direction: Direction, channel: usize, agc: bool) -> Result<(), Error> {
        let gain_mode = if agc == true {
            BladerfGainMode::Default
        } else {
            BladerfGainMode::Mgc
        };
        Ok(block_on(self.inner.set_gain_mode(channel as u8, gain_mode))
            .expect("Could not set gain mode"))
    }

    fn agc(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Ok(block_on(self.inner.get_gain_mode()).is_ok())
    }

    fn gain_elements(&self, _direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        Ok(BladeRf1::get_gain_stages(channel as u8))
    }

    fn set_gain(&self, direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        self.set_gain_element(direction, channel, "IF", gain)
    }

    fn gain(&self, _direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        let gain = block_on(self.inner.get_gain(channel as u8)).expect("Could not retrieve gain");
        Ok(Some(gain as f64))
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
        Ok(
            block_on(self.inner.set_gain_stage(channel as u8, name, gain as i8))
                .expect("Could not set gain"),
        )
    }

    fn gain_element(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        let gain = block_on(self.inner.get_gain_stage(channel as u8, name));
        match gain {
            Ok(g) => Ok(Some(g as f64)),
            Err(_e) => Err(Error::ValueError),
        }
    }

    fn gain_element_range(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Range, Error> {
        // TODO: add support for other gains
        let range =
            BladeRf1::get_gain_stage_range(channel as u8, name).expect("Could not get gain");
        Ok(Range {
            items: vec![RangeItem::Step(
                range.min as f64,
                range.max as f64,
                range.step as f64,
            )],
        })
    }

    fn frequency_range(&self, _direction: Direction, _channel: usize) -> Result<Range, Error> {
        let bladerf1_range = self.inner.get_frequency_range();
        let min_freq = bladerf1_range.min as f64;
        let max_freq = bladerf1_range.max as f64;
        let seify_range = RangeItem::Step(min_freq, max_freq, 1f64);
        Ok(Range::new(vec![seify_range]))
    }

    fn frequency(&self, _direction: Direction, channel: usize) -> Result<f64, Error> {
        match block_on(self.inner.get_frequency(channel as u8)) {
            Ok(f) => Ok(f64::from(f)),
            Err(e) => Err(Error::Misc(e.to_string())),
        }
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
        let x = block_on(self.inner.get_sample_rate(channel as u8));
        if x.is_err() {
            Err(Error::ValueError)
        } else {
            Ok(x.unwrap() as f64)
        }
    }

    fn set_sample_rate(
        &self,
        _direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        let x = block_on(
            self.inner
                .set_sample_rate(channel.try_into().unwrap(), rate as u32),
        );
        if x.is_err() {
            Err(Error::ValueError)
        } else {
            Ok(())
        }
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
        Ok(
            block_on(self.inner.get_bandwidth(channel as u8)).map_err(|_| Error::ValueError)?
                as f64,
        )
    }

    fn set_bandwidth(&self, _direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        Ok(block_on(self.inner.set_bandwidth(channel as u8, bw as u32))
            .map_err(|_| Error::ValueError)?)
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
