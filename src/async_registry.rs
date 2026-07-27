use std::future::Future;

use crate::async_device::{AsyncDeviceInfo, DynAsyncDeviceBackend};
#[cfg(any(
    feature = "dummy",
    all(
        feature = "hydrasdr",
        any(feature = "smol", feature = "tokio"),
        not(target_arch = "wasm32")
    )
))]
use crate::AsyncFutureExt;
use crate::{
    Args, BoxedFuture, Capability, DeviceDescriptor, Driver, DynAsyncDevice, Error, MaybeSend,
    MaybeSync,
};

/// Asynchronous driver discovery/opening backend.
pub trait AsyncDriverBackend: MaybeSend + MaybeSync {
    /// Driver handled by this backend.
    fn driver(&self) -> Driver;
    /// Probe devices matching `args`.
    fn probe<'a>(&'a self, args: &'a Args)
        -> BoxedFuture<'a, Result<Vec<DeviceDescriptor>, Error>>;
    /// Open a previously discovered device descriptor.
    fn open<'a>(
        &'a self,
        descriptor: &'a DeviceDescriptor,
    ) -> BoxedFuture<'a, Result<DynAsyncDevice, Error>>;
}

/// Typed asynchronous driver implementation that can be opened directly.
///
/// Implementations may use `async fn` for `async_probe` and `async_open`; the
/// explicit trait return type enforces Seify's target-dependent `MaybeSend`
/// future bound. Cloning an opened backend must create another handle to the
/// same logical device and shared configuration state.
pub trait AsyncTypedDeviceBackend:
    AsyncDeviceInfo + DynAsyncDeviceBackend + Clone + Sized + 'static
{
    /// Driver implemented by this backend.
    fn driver() -> Driver;
    /// Probe devices matching `args`.
    fn async_probe<'a>(
        args: &'a Args,
    ) -> impl Future<Output = Result<Vec<Args>, Error>> + MaybeSend + 'a;
    /// Open a typed device matching `args`.
    fn async_open<'a>(args: &'a Args)
        -> impl Future<Output = Result<Self, Error>> + MaybeSend + 'a;
}

/// Registry of asynchronous driver discovery/opening backends.
pub struct AsyncRegistry {
    backends: Vec<Box<dyn AsyncDriverBackend>>,
}

impl AsyncRegistry {
    /// Create an empty asynchronous registry.
    pub fn empty() -> Self {
        Self {
            backends: Vec::new(),
        }
    }

    /// Register an asynchronous driver backend.
    pub fn register<B>(&mut self, backend: B) -> &mut Self
    where
        B: AsyncDriverBackend + 'static,
    {
        self.backends.push(Box::new(backend));
        self
    }

    /// Probe devices matching `args`.
    pub async fn probe<'a, A>(&'a self, args: A) -> Result<Vec<DeviceDescriptor>, Error>
    where
        A: TryInto<Args> + MaybeSend + 'a,
    {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let driver = requested_driver(&args)?;
        let mut descriptors = Vec::new();
        let mut matched_backend = false;

        for backend in &self.backends {
            if driver.is_none() || driver == Some(backend.driver()) {
                matched_backend = true;
                descriptors.append(&mut backend.probe(&args).await?);
            }
        }

        if let Some(driver) = driver {
            if !matched_backend {
                if async_builtin_driver_enabled(driver) {
                    return Err(Error::unsupported_reason(
                        Capability::DriverOperation,
                        format!("driver {driver:?} does not expose an async API"),
                    ));
                }
                return Err(Error::DriverFeatureNotEnabled { driver });
            }
        }

        Ok(descriptors)
    }

    /// Open a discovered device descriptor.
    pub async fn open<'a>(
        &'a self,
        descriptor: &'a DeviceDescriptor,
    ) -> Result<DynAsyncDevice, Error> {
        let driver = descriptor.driver();
        let mut matched_backend = false;

        for backend in &self.backends {
            if backend.driver() != driver {
                continue;
            }
            matched_backend = true;
            match backend.open(descriptor).await {
                Ok(device) => return Ok(device),
                Err(Error::DeviceNotFound) => {}
                Err(e) => return Err(e),
            }
        }

        if !matched_backend {
            if async_builtin_driver_enabled(driver) {
                return Err(Error::unsupported_reason(
                    Capability::DriverOperation,
                    format!("driver {driver:?} does not expose an async API"),
                ));
            }
            return Err(Error::DriverFeatureNotEnabled { driver });
        }

        Err(Error::DeviceNotFound)
    }

    /// Open the first asynchronous device matching `args`.
    pub async fn open_args<'a, A>(&'a self, args: A) -> Result<DynAsyncDevice, Error>
    where
        A: TryInto<Args> + MaybeSend + 'a,
    {
        let args = args
            .try_into()
            .map_err(|_| Error::invalid_argument("args", "failed to convert args"))?;
        let driver = requested_driver(&args)?;

        if let Some(driver) = driver {
            let descriptor = DeviceDescriptor::new(driver, args);
            return self.open(&descriptor).await;
        }

        for backend in &self.backends {
            let descriptor = DeviceDescriptor::new(backend.driver(), args.clone());
            match backend.open(&descriptor).await {
                Ok(device) => return Ok(device),
                Err(Error::DeviceNotFound) => {}
                Err(e) => return Err(e),
            }
        }

        Err(Error::DeviceNotFound)
    }
}

impl Default for AsyncRegistry {
    fn default() -> Self {
        #[allow(unused_mut)]
        let mut registry = Self::empty();

        #[cfg(feature = "dummy")]
        registry.register(BuiltinAsyncDriver::<crate::impls::Dummy>::new(
            Driver::Dummy,
        ));

        #[cfg(all(
            feature = "hydrasdr",
            any(feature = "smol", feature = "tokio"),
            not(target_arch = "wasm32")
        ))]
        registry.register(BuiltinAsyncDriver::<crate::impls::AsyncHydraSdr>::new(
            Driver::HydraSdr,
        ));

        registry
    }
}

fn requested_driver(args: &Args) -> Result<Option<Driver>, Error> {
    match args.get::<Driver>("driver") {
        Ok(driver) => Ok(Some(driver)),
        Err(Error::MissingArgument { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

fn async_builtin_driver_enabled(driver: Driver) -> bool {
    match driver {
        Driver::AaroniaHttp => cfg!(all(feature = "aaronia_http", not(target_arch = "wasm32"))),
        Driver::BladeRf => cfg!(all(feature = "bladerf1", not(target_arch = "wasm32"))),
        Driver::Dummy => cfg!(feature = "dummy"),
        Driver::HackRf => cfg!(all(feature = "hackrfone", not(target_arch = "wasm32"))),
        Driver::HydraSdr => cfg!(all(
            feature = "hydrasdr",
            any(feature = "smol", feature = "tokio"),
            not(target_arch = "wasm32")
        )),
        Driver::RtlSdr => cfg!(all(feature = "rtlsdr", not(target_arch = "wasm32"))),
        Driver::Soapy => cfg!(all(feature = "soapy", not(target_arch = "wasm32"))),
    }
}

#[cfg(any(
    feature = "dummy",
    all(
        feature = "hydrasdr",
        any(feature = "smol", feature = "tokio"),
        not(target_arch = "wasm32")
    )
))]
struct BuiltinAsyncDriver<D> {
    driver: Driver,
    _device: std::marker::PhantomData<D>,
}

#[cfg(any(
    feature = "dummy",
    all(
        feature = "hydrasdr",
        any(feature = "smol", feature = "tokio"),
        not(target_arch = "wasm32")
    )
))]
impl<D> BuiltinAsyncDriver<D> {
    fn new(driver: Driver) -> Self {
        Self {
            driver,
            _device: std::marker::PhantomData,
        }
    }
}

#[cfg(any(
    feature = "dummy",
    all(
        feature = "hydrasdr",
        any(feature = "smol", feature = "tokio"),
        not(target_arch = "wasm32")
    )
))]
impl<D> AsyncDriverBackend for BuiltinAsyncDriver<D>
where
    D: AsyncTypedDeviceBackend + MaybeSend + MaybeSync,
{
    fn driver(&self) -> Driver {
        self.driver
    }

    fn probe<'a>(
        &'a self,
        args: &'a Args,
    ) -> BoxedFuture<'a, Result<Vec<DeviceDescriptor>, Error>> {
        async move {
            D::async_probe(args).await.map(|descriptors| {
                descriptors
                    .into_iter()
                    .map(|args| DeviceDescriptor::new(self.driver, args))
                    .collect()
            })
        }
        .boxed_async()
    }

    fn open<'a>(
        &'a self,
        descriptor: &'a DeviceDescriptor,
    ) -> BoxedFuture<'a, Result<DynAsyncDevice, Error>> {
        async move {
            Ok(DynAsyncDevice::from_impl(
                D::async_open(descriptor.args()).await?,
            ))
        }
        .boxed_async()
    }
}

#[cfg(all(
    test,
    feature = "hydrasdr",
    not(any(feature = "smol", feature = "tokio")),
    not(target_arch = "wasm32")
))]
mod tests {
    use super::*;
    use futures::executor::block_on;

    #[test]
    fn async_registry_reports_disabled_hydrasdr_without_runtime_feature() {
        block_on(async {
            let registry = AsyncRegistry::default();

            assert!(matches!(
                registry.probe("driver=hydrasdr").await,
                Err(Error::DriverFeatureNotEnabled {
                    driver: Driver::HydraSdr
                })
            ));
            assert!(matches!(
                registry.open_args("driver=hydrasdr").await,
                Err(Error::DriverFeatureNotEnabled {
                    driver: Driver::HydraSdr
                })
            ));
        });
    }
}
