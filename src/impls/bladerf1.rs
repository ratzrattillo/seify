use crate::{Args, Direction, Error, Range, RangeItem};
use bladerf_globals::bladerf1::BladerfXb::BladerfXb200;
use bladerf_globals::bladerf1::{BladerfXb, BLADERF_FREQUENCY_MIN};
use bladerf_globals::{BladeRfDirection, BladerfFormat, BladerfGainMode, BLADERF_MODULE_RX};
use futures::executor::block_on;
use libbladerf_rs::board::bladerf1::BladeRf1;
use num_complex::Complex32;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

pub struct BladeRf {
    inner: Arc<Mutex<BladeRf1>>,
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
                    "driver=bladerf, bus_number={}, address={}",
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

        log::debug!("args: {:?}", args);
        if let Ok(fd) = args.get::<i32>("fd") {
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let mut bladerf =
                *block_on(BladeRf1::from_fd(fd)).map_err(|e| Error::Misc(e.to_string()))?;
            let _initialized =
                block_on(bladerf.initialize()).map_err(|e| Error::Misc(e.to_string()))?;
            return Ok(Self {
                inner: Arc::new(Mutex::new(bladerf)),
            });
        }

        let bus_number = args.get("bus_number");
        let address = args.get("address");
        let dev = match (bus_number, address) {
            (Ok(bus_number), Ok(address)) => {
                let mut bladerf = block_on(BladeRf1::from_bus_addr(bus_number, address))
                    .map_err(|e| Error::Misc(e.to_string()))?;
                let _initialized =
                    block_on((*bladerf).initialize()).map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
            }
            (Err(Error::NotFound), Err(Error::NotFound)) => {
                log::debug!("Opening first bladerf device");
                let mut bladerf =
                    block_on(BladeRf1::from_first()).map_err(|e| Error::Misc(e.to_string()))?;
                let _initialized =
                    block_on((*bladerf).initialize()).map_err(|e| Error::Misc(e.to_string()))?;
                bladerf
            }
            (bus_number, address) => {
                log::warn!("BladeRf::open received invalid args: bus_number: {bus_number:?}, address: {address:?}");
                return Err(Error::ValueError);
            }
        };

        Ok(Self {
            inner: Arc::new(Mutex::new(*dev)),
        })
    }

    pub fn enable_expansion_board(&mut self, board_type: BladerfXb) -> Result<(), Error> {
        block_on(self.inner.lock().unwrap().expansion_attach(board_type))
            .map_err(|e| Error::Misc(e.to_string()))
    }
}

// TODO Adjust to proper value
const MTU: usize = 4 * 16384;

pub struct RxStreamer {
    dev: Arc<Mutex<BladeRf1>>,
}

pub struct TxStreamer {
    #[allow(dead_code)]
    dev: Arc<Mutex<BladeRf1>>,
}

impl crate::RxStreamer for RxStreamer {
    fn mtu(&self) -> Result<usize, Error> {
        Ok(MTU)
    }

    fn activate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        let handle = self.dev.lock().unwrap();
        block_on(handle.perform_format_config(BladeRfDirection::Rx, BladerfFormat::Sc16Q11))
            .map_err(|e| Error::Misc(e.to_string()))?;
        block_on(handle.enable_module(BLADERF_MODULE_RX, true))
            .map_err(|e| Error::Misc(e.to_string()))?;
        block_on(handle.experimental_control_urb()).map_err(|e| Error::Misc(e.to_string()))?;
        Ok(())
    }

    fn deactivate_at(&mut self, _time_ns: Option<i64>) -> Result<(), Error> {
        Err(Error::NotSupported)
    }

    fn read(&mut self, buffers: &mut [&mut [Complex32]], timeout_us: i64) -> Result<usize, Error> {
        Ok(self
            .dev
            .lock()
            .unwrap()
            .read_sync(buffers, timeout_us)
            .map_err(|e| Error::Misc(e.to_string()))?)
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
        block_on(self.inner.lock().unwrap().serial()).map_err(|e| Error::Misc(e.to_string()))
    }

    fn info(&self) -> Result<Args, Error> {
        let mut args = Args::default();
        let fw_version = block_on(self.inner.lock().unwrap().fx3_firmware())
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
            log::debug!("BladeRF1 only supports one RX channel!");
            Err(Error::ValueError)
        } else {
            Ok(RxStreamer {
                dev: self.inner.clone(),
            })
        }
    }

    fn tx_streamer(&self, channels: &[usize], _args: Args) -> Result<Self::TxStreamer, Error> {
        if channels != [0] {
            log::debug!("BladeRF1 only supports one TX channel!");
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
        Ok(block_on(self.inner.lock().unwrap().get_gain_modes(channel as u8)).is_ok())
    }

    fn enable_agc(&self, _direction: Direction, channel: usize, agc: bool) -> Result<(), Error> {
        let gain_mode = if agc == true {
            BladerfGainMode::Default
        } else {
            BladerfGainMode::Mgc
        };
        block_on(
            self.inner
                .lock()
                .unwrap()
                .set_gain_mode(channel as u8, gain_mode),
        )
        .map_err(|e| Error::Misc(e.to_string()))
    }

    fn agc(&self, _direction: Direction, _channel: usize) -> Result<bool, Error> {
        Ok(block_on(self.inner.lock().unwrap().get_gain_mode()).is_ok())
    }

    fn gain_elements(&self, _direction: Direction, channel: usize) -> Result<Vec<String>, Error> {
        Ok(BladeRf1::get_gain_stages(channel as u8))
    }

    fn set_gain(&self, _direction: Direction, channel: usize, gain: f64) -> Result<(), Error> {
        Ok(block_on(
            self.inner
                .lock()
                .unwrap()
                .set_gain(channel as u8, gain as i8),
        )
        .map_err(|e| Error::Misc(e.to_string()))?)
    }

    fn gain(&self, _direction: Direction, channel: usize) -> Result<Option<f64>, Error> {
        Ok(Some(
            block_on(self.inner.lock().unwrap().get_gain(channel as u8))
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
        block_on(
            self.inner
                .lock()
                .unwrap()
                .set_gain_stage(channel as u8, name, gain as i8),
        )
        .map_err(|e| Error::Misc(e.to_string()))
    }

    fn gain_element(
        &self,
        _direction: Direction,
        channel: usize,
        name: &str,
    ) -> Result<Option<f64>, Error> {
        Ok(Some(
            block_on(
                self.inner
                    .lock()
                    .unwrap()
                    .get_gain_stage(channel as u8, name),
            )
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
        let bladerf1_range = self.inner.lock().unwrap().get_frequency_range();
        let min_freq = bladerf1_range.min as f64;
        let max_freq = bladerf1_range.max as f64;
        let seify_range = RangeItem::Step(min_freq, max_freq, 1f64);
        Ok(Range::new(vec![seify_range]))
    }

    fn frequency(&self, _direction: Direction, channel: usize) -> Result<f64, Error> {
        Ok(
            block_on(self.inner.lock().unwrap().get_frequency(channel as u8))
                .map_err(|e| Error::Misc(e.to_string()))? as f64,
        )
    }

    fn set_frequency(
        &self,
        _direction: Direction,
        channel: usize,
        frequency: f64,
        _args: Args,
    ) -> Result<(), Error> {
        if frequency < BLADERF_FREQUENCY_MIN as f64 {
            log::debug!("Frequency {frequency} requires XB200 expansion board");
            let xb = self.inner.lock().unwrap().expansion_get_attached();
            if xb != BladerfXb200 {
                log::debug!("Automatically attaching XB200 expansion board");
                block_on(self.inner.lock().unwrap().expansion_attach(BladerfXb200))
                    .map_err(|e| Error::Misc(e.to_string()))?;
            }
        }
        log::debug!("Setting frequency to {}", frequency);
        block_on(
            self.inner
                .lock()
                .unwrap()
                .set_frequency(channel as u8, frequency as u64),
        )
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
        Ok(
            block_on(self.inner.lock().unwrap().get_sample_rate(channel as u8))
                .map_err(|e| Error::Misc(e.to_string()))? as f64,
        )
    }

    fn set_sample_rate(
        &self,
        _direction: Direction,
        channel: usize,
        rate: f64,
    ) -> Result<(), Error> {
        block_on(
            self.inner
                .lock()
                .unwrap()
                .set_sample_rate(channel.try_into().unwrap(), rate as u32),
        )
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
        Ok(
            block_on(self.inner.lock().unwrap().get_bandwidth(channel as u8))
                .map_err(|e| Error::Misc(e.to_string()))? as f64,
        )
    }

    fn set_bandwidth(&self, _direction: Direction, channel: usize, bw: f64) -> Result<(), Error> {
        Ok(block_on(
            self.inner
                .lock()
                .unwrap()
                .set_bandwidth(channel as u8, bw as u32),
        )
        .map_err(|e| Error::Misc(e.to_string()))?)
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
