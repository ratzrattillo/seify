use hydrasdr_rs::{
    ActiveState, Bandwidth, DeviceDescriptor, DeviceInfo, ErrorKind, GainConfig, GainStage, RfPort,
};

use crate::Direction::*;
use crate::{Args, Capability, Direction, Error, Range, RangeItem};

pub(super) const F32_RX_MTU: usize = hydrasdr_rs::MAX_F32_IQ_SAMPLES_PER_TRANSFER;
const LNA_GAIN_MAX_DB: u8 = 14;
const MIXER_GAIN_MAX_DB: u8 = 15;
const VGA_GAIN_MAX_DB: u8 = 15;
pub(super) struct ReceiverContext {
    pub(super) active: ActiveState,
    pub(super) sample_rates: Vec<u32>,
    pub(super) bandwidths: Vec<u32>,
    pub(super) gains: Vec<GainElement>,
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

impl ReceiverContext {
    pub(super) fn from_device_info(
        info: &DeviceInfo,
        sample_rates: Vec<u32>,
        bandwidths: Vec<u32>,
    ) -> Self {
        Self {
            active: info.active_state.clone(),
            sample_rates,
            bandwidths,
            gains: gain_elements(),
            min_frequency: info.min_frequency as f64,
            max_frequency: info.max_frequency as f64,
        }
    }

    pub(super) fn antenna(&self) -> Result<&'static str, Error> {
        Ok(match self.active.rf_port().map_err(map_hydrasdr_error)? {
            RfPort::Rx0 => "ANT",
            RfPort::Rx1 => "CABLE1",
            RfPort::Rx2 => "CABLE2",
        })
    }

    pub(super) fn frequency(&self) -> Result<f64, Error> {
        self.active
            .frequency_hz()
            .map(|value| value as f64)
            .map_err(map_hydrasdr_error)
    }

    pub(super) fn sample_rate(&self) -> Result<f64, Error> {
        self.active
            .sample_rate_hz()
            .map(|value| value as f64)
            .map_err(map_hydrasdr_error)
    }

    pub(super) fn bandwidth(&self) -> Result<f64, Error> {
        match self.active.bandwidth().map_err(map_hydrasdr_error)? {
            Bandwidth::ManualHz(value) => Ok(value as f64),
            Bandwidth::Auto => Err(Error::unsupported(Capability::DriverOperation)),
        }
    }

    pub(super) fn agc_enabled(&self) -> Result<bool, Error> {
        self.active.agc_enabled().map_err(map_hydrasdr_error)
    }

    pub(super) fn gain_value(&self, gain_type: GainType) -> Result<Option<f64>, Error> {
        self.active
            .gain(gain_type.stage())
            .map(|value| value.map(f64::from))
            .map_err(map_hydrasdr_error)
    }

    pub(super) fn overall_gain(&self) -> Result<Option<f64>, Error> {
        [GainType::Lna, GainType::Mixer, GainType::Vga]
            .into_iter()
            .try_fold(Some(0.0), |sum, gain_type| {
                Ok(match (sum, self.gain_value(gain_type)?) {
                    (Some(sum), Some(value)) => Some(sum + value),
                    _ => None,
                })
            })
    }

    pub(super) fn gain_range(&self, gain_type: GainType) -> Option<Range> {
        self.gains
            .iter()
            .find(|element| element.gain_type == gain_type)
            .map(|element| element.range.clone())
    }
}

pub(super) struct GainElement {
    pub(super) name: &'static str,
    pub(super) gain_type: GainType,
    pub(super) range: Range,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum GainType {
    Lna,
    Mixer,
    Vga,
}

impl GainType {
    fn stage(self) -> GainStage {
        match self {
            Self::Lna => GainStage::Lna,
            Self::Mixer => GainStage::Mixer,
            Self::Vga => GainStage::Vga,
        }
    }

    pub(super) fn update(self, gain: f64) -> GainConfig {
        let gain = gain.round() as u8;
        match self {
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

pub(super) fn agc_gain_config(enabled: bool) -> GainConfig {
    GainConfig::Manual {
        lna: None,
        mixer: None,
        vga: None,
        lna_agc: Some(enabled),
        mixer_agc: Some(enabled),
    }
}

pub(super) fn overall_gain_range() -> Range {
    let max = LNA_GAIN_MAX_DB + MIXER_GAIN_MAX_DB + VGA_GAIN_MAX_DB;
    Range::new(vec![RangeItem::Step(0.0, f64::from(max), 1.0)])
}

pub(super) fn distribute_overall_gain(mut gain: f64) -> [(GainType, f64); 3] {
    let lna = gain.min(f64::from(LNA_GAIN_MAX_DB));
    gain -= lna;
    let mixer = gain.min(f64::from(MIXER_GAIN_MAX_DB));
    gain -= mixer;
    let vga = gain.min(f64::from(VGA_GAIN_MAX_DB));
    [
        (GainType::Lna, lna),
        (GainType::Mixer, mixer),
        (GainType::Vga, vga),
    ]
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
        _ => None,
    }
}

pub(super) fn gain_elements() -> Vec<GainElement> {
    [
        ("LNA", GainType::Lna, 0, LNA_GAIN_MAX_DB),
        ("MIXER", GainType::Mixer, 0, MIXER_GAIN_MAX_DB),
        ("VGA", GainType::Vga, 0, VGA_GAIN_MAX_DB),
    ]
    .into_iter()
    .map(|(name, gain_type, min_value, max_value)| {
        gain_element(name, gain_type, min_value, max_value, 1)
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
pub(super) fn gain_element(
    name: &'static str,
    gain_type: GainType,
    min_value: u8,
    max_value: u8,
    step_value: u8,
) -> GainElement {
    let step = step_value.max(1) as f64;
    GainElement {
        name,
        gain_type,
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
    fn generic_gain_elements_are_physical_stages() {
        let gains = gain_elements();
        let names = gains.iter().map(|gain| gain.name).collect::<Vec<_>>();

        assert_eq!(names, ["LNA", "MIXER", "VGA"]);
        assert_eq!(gain_type("linearity"), None);
        assert_eq!(gain_type("sensitivity"), None);
    }

    #[test]
    fn overall_gain_requires_authoritative_physical_stage_values() {
        let state = ReceiverContext {
            active: ActiveState::default(),
            sample_rates: Vec::new(),
            bandwidths: Vec::new(),
            gains: gain_elements(),
            min_frequency: 0.0,
            max_frequency: 0.0,
        };

        assert_eq!(state.overall_gain().unwrap(), None);
    }

    #[test]
    fn overall_gain_is_distributed_from_rf_to_baseband() {
        assert_eq!(
            distribute_overall_gain(0.0),
            [
                (GainType::Lna, 0.0),
                (GainType::Mixer, 0.0),
                (GainType::Vga, 0.0),
            ]
        );
        assert_eq!(
            distribute_overall_gain(20.0),
            [
                (GainType::Lna, 14.0),
                (GainType::Mixer, 6.0),
                (GainType::Vga, 0.0),
            ]
        );
        assert_eq!(
            distribute_overall_gain(44.0),
            [
                (GainType::Lna, 14.0),
                (GainType::Mixer, 15.0),
                (GainType::Vga, 15.0),
            ]
        );

        let range = overall_gain_range();
        assert!(range.contains(0.0));
        assert!(range.contains(44.0));
        assert!(!range.contains(10.5));
        assert!(!range.contains(45.0));
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
