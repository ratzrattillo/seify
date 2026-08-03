use hydrasdr_rs::{
    Bandwidth, DeviceDescriptor, DeviceInfo, ErrorKind, GainConfig, GainPreset, RfPort,
};

use crate::Direction::*;
use crate::{Args, Capability, Direction, Error, Range, RangeItem};

pub(super) const F32_RX_MTU: usize = hydrasdr_rs::MAX_F32_IQ_SAMPLES_PER_TRANSFER;
pub(super) struct ReceiverState {
    pub(super) antenna: &'static str,
    pub(super) frequency: Option<f64>,
    pub(super) sample_rate: Option<f64>,
    pub(super) bandwidth: Option<f64>,
    pub(super) sample_rates: Vec<u32>,
    pub(super) bandwidths: Vec<u32>,
    pub(super) gains: Vec<GainCache>,
    pub(super) agc: bool,
    pub(super) min_frequency: f64,
    pub(super) max_frequency: f64,
}

pub(super) fn bandwidth_range(bandwidths: &[u32]) -> Result<Range, Error> {
    discrete_range(bandwidths, Capability::Bandwidth)
}

pub(super) fn sample_rate_range(sample_rates: &[u32]) -> Result<Range, Error> {
    discrete_range(sample_rates, Capability::SampleRate)
}

fn discrete_range(values: &[u32], capability: Capability) -> Result<Range, Error> {
    if values.is_empty() {
        return Err(Error::unsupported(capability));
    }
    Ok(Range::new(
        values
            .iter()
            .map(|value| RangeItem::Value(*value as f64))
            .collect(),
    ))
}

impl ReceiverState {
    pub(super) fn from_device_info(
        info: &DeviceInfo,
        sample_rates: Vec<u32>,
        bandwidths: Vec<u32>,
    ) -> Self {
        let current_config = info.current_config.as_ref();
        Self {
            antenna: "ANT",
            frequency: current_config.map(|config| config.frequency_hz() as f64),
            sample_rate: current_config
                .map(|config| config.sample_rate_hz() as f64)
                .or_else(|| sample_rates.first().map(|rate| *rate as f64)),
            bandwidth: current_config
                .and_then(|config| match config.bandwidth() {
                    Bandwidth::Auto => None,
                    Bandwidth::ManualHz(bandwidth) => Some(bandwidth as f64),
                })
                .or_else(|| bandwidths.first().map(|bandwidth| *bandwidth as f64)),
            sample_rates,
            bandwidths,
            gains: default_gain_cache(),
            agc: false,
            min_frequency: info.min_frequency as f64,
            max_frequency: info.max_frequency as f64,
        }
    }

    pub(super) fn set_agc_cached(&mut self, agc: bool) {
        self.agc = agc;
    }

    pub(super) fn set_gain_cached(&mut self, gain_type: GainType, value: f64) {
        if let Some(cached) = self
            .gains
            .iter_mut()
            .find(|cached| cached.gain_type == gain_type)
        {
            cached.value = value;
        }
    }

    pub(super) fn gain_value(&self, gain_type: GainType) -> Option<f64> {
        self.gains
            .iter()
            .find(|cached| cached.gain_type == gain_type)
            .map(|cached| cached.value)
    }

    pub(super) fn gain_range(&self, gain_type: GainType) -> Option<Range> {
        self.gains
            .iter()
            .find(|cached| cached.gain_type == gain_type)
            .map(|cached| cached.range.clone())
    }
}

pub(super) struct GainCache {
    pub(super) name: &'static str,
    pub(super) gain_type: GainType,
    pub(super) value: f64,
    pub(super) range: Range,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GainType {
    Lna,
    Mixer,
    Vga,
    Linearity,
    Sensitivity,
}

impl GainType {
    pub(super) fn update(self, gain: f64) -> GainConfig {
        let gain = gain.round() as u8;
        match self {
            Self::Linearity => GainConfig::Preset(GainPreset::Linearity(gain)),
            Self::Sensitivity => GainConfig::Preset(GainPreset::Sensitivity(gain)),
            Self::Lna => GainConfig::Manual {
                lna: Some(gain),
                mixer: None,
                vga: None,
                lna_agc: None,
                mixer_agc: None,
            },
            Self::Mixer => GainConfig::Manual {
                lna: None,
                mixer: Some(gain),
                vga: None,
                lna_agc: None,
                mixer_agc: None,
            },
            Self::Vga => GainConfig::Manual {
                lna: None,
                mixer: None,
                vga: Some(gain),
                lna_agc: None,
                mixer_agc: None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DeviceSelector {
    First,
    Serial(u64),
    Index(usize),
}

pub(super) fn check_rx(direction: Direction, channel: usize) -> Result<(), Error> {
    if matches!(direction, Rx) && channel == 0 {
        Ok(())
    } else if matches!(direction, Rx) {
        Err(Error::invalid_channel(Direction::Rx, channel, 1))
    } else {
        Err(Error::unsupported(Capability::RxStreaming))
    }
}

pub(super) fn antenna_port(name: &str) -> Option<(&'static str, RfPort)> {
    match name.to_ascii_uppercase().as_str() {
        "ANT" => Some(("ANT", RfPort::Rx0)),
        "CABLE1" => Some(("CABLE1", RfPort::Rx1)),
        "CABLE2" => Some(("CABLE2", RfPort::Rx2)),
        _ => None,
    }
}

pub(super) fn gain_type(name: &str) -> Option<GainType> {
    match name.to_ascii_uppercase().as_str() {
        "LNA" => Some(GainType::Lna),
        "MIXER" => Some(GainType::Mixer),
        "VGA" => Some(GainType::Vga),
        "LINEARITY" => Some(GainType::Linearity),
        "SENSITIVITY" => Some(GainType::Sensitivity),
        _ => None,
    }
}

pub(super) fn default_gain_cache() -> Vec<GainCache> {
    [
        ("LNA", GainType::Lna, 0, 14, 8),
        ("MIXER", GainType::Mixer, 0, 15, 8),
        ("VGA", GainType::Vga, 0, 15, 8),
        ("LINEARITY", GainType::Linearity, 0, 21, 10),
        ("SENSITIVITY", GainType::Sensitivity, 0, 21, 10),
    ]
    .into_iter()
    .map(|(name, gain_type, min_value, max_value, value)| {
        gain_cache_item(name, gain_type, min_value, max_value, 1, value)
    })
    .collect()
}

pub(super) fn probe_args_from_info(dev: DeviceDescriptor) -> Args {
    let mut args = Args::default();
    args.set("driver", "hydrasdr");
    args.set("vid", format!("0x{:04x}", dev.vid));
    args.set("pid", format!("0x{:04x}", dev.pid));
    args.set("description", dev.description);
    if let Some(serial) = dev.serial {
        args.set("serial", serial.to_string());
    }
    if let Some(product) = dev.product_string {
        args.set("product", product);
    }
    args
}

pub(super) fn device_selector(args: &Args) -> Result<DeviceSelector, Error> {
    match args.get::<usize>("index") {
        Ok(index) => return Ok(DeviceSelector::Index(index)),
        Err(Error::MissingArgument { .. }) => {}
        Err(err) => return Err(err),
    }

    match args.get::<u64>("serial") {
        Ok(serial) => Ok(DeviceSelector::Serial(serial)),
        Err(Error::MissingArgument { .. }) => Ok(DeviceSelector::First),
        Err(err) => Err(err),
    }
}
pub(super) fn gain_cache_item(
    name: &'static str,
    gain_type: GainType,
    min_value: u8,
    max_value: u8,
    step_value: u8,
    value: u8,
) -> GainCache {
    let step = step_value.max(1) as f64;
    GainCache {
        name,
        gain_type,
        value: value as f64,
        range: Range::new(vec![RangeItem::Step(
            min_value as f64,
            max_value as f64,
            step,
        )]),
    }
}

pub(super) fn map_hydrasdr_error(err: hydrasdr_rs::Error) -> Error {
    match err.kind() {
        ErrorKind::InvalidConfig => Error::invalid_argument("hydrasdr", err.to_string()),
        ErrorKind::NotFound => Error::DeviceNotFound,
        ErrorKind::Busy => Error::Busy,
        ErrorKind::Unsupported => Error::unsupported(Capability::DriverOperation),
        ErrorKind::StreamClosed => Error::StreamClosed,
        _ => err.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_bandwidth_list_is_unsupported() {
        assert!(bandwidth_range(&[]).unwrap_err().is_unsupported());
    }

    #[test]
    fn bandwidth_range_contains_reported_values_only() {
        let range = bandwidth_range(&[1_750_000, 2_500_000]).unwrap();
        assert!(range.contains(1_750_000.0));
        assert!(range.contains(2_500_000.0));
        assert!(!range.contains(2_000_000.0));
    }

    #[test]
    fn empty_sample_rate_list_is_unsupported() {
        assert!(sample_rate_range(&[]).unwrap_err().is_unsupported());
    }

    #[test]
    fn sample_rate_range_contains_reported_values_only() {
        let range = sample_rate_range(&[2_500_000, 10_000_000]).unwrap();
        assert!(range.contains(2_500_000.0));
        assert!(range.contains(10_000_000.0));
        assert!(!range.contains(5_000_000.0));
    }

    #[test]
    fn probe_args_from_info_maps_usb_metadata_without_opening_hardware() {
        let info = DeviceDescriptor {
            vid: 0x38af,
            pid: 0x0001,
            description: "HydraSDR RFOne Official VID/PID",
            serial: Some(0x1234_5678_9abc_def0),
            product_string: Some("HydraSDR RFOne".to_string()),
        };

        let args = probe_args_from_info(info);

        assert_eq!(args.get::<String>("driver").unwrap(), "hydrasdr");
        assert_eq!(args.get::<String>("vid").unwrap(), "0x38af");
        assert_eq!(args.get::<String>("pid").unwrap(), "0x0001");
        assert_eq!(
            args.get::<String>("description").unwrap(),
            "HydraSDR RFOne Official VID/PID"
        );
        assert_eq!(args.get::<String>("serial").unwrap(), "1311768467463790320");
        assert_eq!(args.get::<String>("product").unwrap(), "HydraSDR RFOne");
    }

    #[test]
    fn check_rx_accepts_only_rx_channel_zero_and_rejects_tx() {
        assert!(check_rx(Rx, 0).is_ok());
        assert!(matches!(
            check_rx(Rx, 1),
            Err(Error::InvalidChannel {
                direction: Rx,
                channel: 1,
                available: 1,
            })
        ));
        assert!(matches!(
            check_rx(Tx, 0),
            Err(Error::Unsupported {
                capability: Capability::RxStreaming,
                ..
            })
        ));
    }

    #[test]
    fn device_selector_defaults_to_first_device() {
        let args = Args::default();

        assert_eq!(device_selector(&args).unwrap(), DeviceSelector::First);
    }

    #[test]
    fn device_selector_accepts_serial() {
        let args: Args = "driver=hydrasdr,serial=1234".try_into().unwrap();

        assert_eq!(
            device_selector(&args).unwrap(),
            DeviceSelector::Serial(1234)
        );
    }

    #[test]
    fn device_selector_prefers_index_over_serial_like_other_seify_drivers() {
        let args: Args = "driver=hydrasdr,index=2,serial=1234".try_into().unwrap();

        assert_eq!(device_selector(&args).unwrap(), DeviceSelector::Index(2));
    }

    #[test]
    fn device_selector_rejects_invalid_index_and_serial_args() {
        let bad_index: Args = "driver=hydrasdr,index=not-a-number".try_into().unwrap();
        assert!(matches!(
            device_selector(&bad_index),
            Err(Error::InvalidArgument { name, .. }) if name == "index"
        ));

        let bad_serial: Args = "driver=hydrasdr,serial=not-a-number".try_into().unwrap();
        assert!(matches!(
            device_selector(&bad_serial),
            Err(Error::InvalidArgument { name, .. }) if name == "serial"
        ));
    }
}
