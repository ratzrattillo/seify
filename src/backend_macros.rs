/// Implement runtime dispatch for a synchronous device backend.
///
/// The listed capabilities must be implemented by the backend's concrete type.
/// Supported capability names are `rx`, `tx`, `antenna`, `agc`, `gain`,
/// `frequency`, `sample_rate`, `bandwidth`, and `dc_offset`.
#[macro_export]
macro_rules! impl_dyn_device_backend {
    ($device:ty => [$($capability:ident),* $(,)?]) => {
        impl $crate::dev::DynDeviceBackend for $device {
            $(
                $crate::__seify_dyn_device_capability!($capability);
            )*
        }
    };
}

/// Implement runtime dispatch for an asynchronous device backend.
///
/// The listed capabilities must be implemented by the backend's concrete type.
/// Supported capability names are `rx`, `tx`, `antenna`, `agc`, `gain`,
/// `frequency`, `sample_rate`, `bandwidth`, and `dc_offset`.
#[macro_export]
macro_rules! impl_dyn_async_device_backend {
    ($device:ty => [$($capability:ident),* $(,)?]) => {
        impl $crate::dev::DynAsyncDeviceBackend for $device {
            $(
                $crate::__seify_dyn_async_device_capability!($capability);
            )*
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __seify_dyn_device_capability {
    (rx) => {
        fn rx_device(&self) -> Option<&dyn $crate::dev::DynRxDevice> {
            Some(self)
        }
    };
    (tx) => {
        fn tx_device(&self) -> Option<&dyn $crate::dev::DynTxDevice> {
            Some(self)
        }
    };
    (antenna) => {
        fn antenna_control(&self) -> Option<&dyn $crate::AntennaControl> {
            Some(self)
        }
    };
    (agc) => {
        fn agc_control(&self) -> Option<&dyn $crate::AgcControl> {
            Some(self)
        }
    };
    (gain) => {
        fn gain_control(&self) -> Option<&dyn $crate::GainControl> {
            Some(self)
        }
    };
    (frequency) => {
        fn frequency_control(&self) -> Option<&dyn $crate::FrequencyControl> {
            Some(self)
        }
    };
    (sample_rate) => {
        fn sample_rate_control(&self) -> Option<&dyn $crate::SampleRateControl> {
            Some(self)
        }
    };
    (bandwidth) => {
        fn bandwidth_control(&self) -> Option<&dyn $crate::BandwidthControl> {
            Some(self)
        }
    };
    (dc_offset) => {
        fn dc_offset_control(&self) -> Option<&dyn $crate::DcOffsetControl> {
            Some(self)
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __seify_dyn_async_device_capability {
    (rx) => {
        fn async_rx_device(&self) -> Option<&dyn $crate::dev::DynAsyncRxDevice> {
            Some(self)
        }
    };
    (tx) => {
        fn async_tx_device(&self) -> Option<&dyn $crate::dev::DynAsyncTxDevice> {
            Some(self)
        }
    };
    (antenna) => {
        fn async_antenna_control(&self) -> Option<&dyn $crate::dev::DynAsyncAntennaControl> {
            Some(self)
        }
    };
    (agc) => {
        fn async_agc_control(&self) -> Option<&dyn $crate::dev::DynAsyncAgcControl> {
            Some(self)
        }
    };
    (gain) => {
        fn async_gain_control(&self) -> Option<&dyn $crate::dev::DynAsyncGainControl> {
            Some(self)
        }
    };
    (frequency) => {
        fn async_frequency_control(&self) -> Option<&dyn $crate::dev::DynAsyncFrequencyControl> {
            Some(self)
        }
    };
    (sample_rate) => {
        fn async_sample_rate_control(&self) -> Option<&dyn $crate::dev::DynAsyncSampleRateControl> {
            Some(self)
        }
    };
    (bandwidth) => {
        fn async_bandwidth_control(&self) -> Option<&dyn $crate::dev::DynAsyncBandwidthControl> {
            Some(self)
        }
    };
    (dc_offset) => {
        fn async_dc_offset_control(&self) -> Option<&dyn $crate::dev::DynAsyncDcOffsetControl> {
            Some(self)
        }
    };
}
