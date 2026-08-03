use std::future::Future;

use crate::async_device::{AsyncDeviceInfo, DynAsyncDeviceBackend};
use crate::AsyncFutureExt;
use crate::{
    Args, BoxedFuture, Capability, DeviceDescriptor, Driver, DynAsyncDevice, Error, MaybeSend,
};

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

type ProbeFuture<'a> = BoxedFuture<'a, Result<Vec<DeviceDescriptor>, Error>>;
type OpenFuture<'a> = BoxedFuture<'a, Result<DynAsyncDevice, Error>>;

struct RegisteredAsyncDriver {
    driver: Driver,
    probe: for<'a> fn(&'a Args) -> ProbeFuture<'a>,
    open: for<'a> fn(&'a DeviceDescriptor) -> OpenFuture<'a>,
}

impl RegisteredAsyncDriver {
    fn new<D: AsyncTypedDeviceBackend>() -> Self {
        Self {
            driver: <D as AsyncTypedDeviceBackend>::driver(),
            probe: probe_typed::<D>,
            open: open_typed::<D>,
        }
    }

    fn driver(&self) -> Driver {
        self.driver
    }

    fn probe<'a>(&self, args: &'a Args) -> ProbeFuture<'a> {
        (self.probe)(args)
    }

    fn open<'a>(&self, descriptor: &'a DeviceDescriptor) -> OpenFuture<'a> {
        (self.open)(descriptor)
    }
}

fn probe_typed<D: AsyncTypedDeviceBackend>(args: &Args) -> ProbeFuture<'_> {
    async move {
        D::async_probe(args).await.map(|descriptors| {
            descriptors
                .into_iter()
                .map(|args| DeviceDescriptor::new(<D as AsyncTypedDeviceBackend>::driver(), args))
                .collect()
        })
    }
    .boxed_async()
}

fn open_typed<D: AsyncTypedDeviceBackend>(descriptor: &DeviceDescriptor) -> OpenFuture<'_> {
    async move {
        Ok(DynAsyncDevice::from_impl(
            D::async_open(descriptor.args()).await?,
        ))
    }
    .boxed_async()
}

/// Registry of asynchronous driver discovery/opening backends.
pub struct AsyncRegistry {
    backends: Vec<RegisteredAsyncDriver>,
}

impl AsyncRegistry {
    /// Create an empty asynchronous registry.
    pub fn empty() -> Self {
        Self {
            backends: Vec::new(),
        }
    }

    /// Register a typed built-in asynchronous driver.
    pub fn register<D>(&mut self) -> &mut Self
    where
        D: AsyncTypedDeviceBackend,
    {
        self.backends.push(RegisteredAsyncDriver::new::<D>());
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
                return Err(unavailable_driver(driver));
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
            return Err(unavailable_driver(driver));
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

        #[cfg(all(
            feature = "hydrasdr",
            any(target_arch = "wasm32", feature = "smol", feature = "tokio")
        ))]
        registry.register::<crate::impls::AsyncHydraSdr>();

        #[cfg(feature = "dummy")]
        registry.register::<crate::impls::Dummy>();

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

fn unavailable_driver(driver: Driver) -> Error {
    if !matches!(driver, Driver::Dummy | Driver::HydraSdr)
        && crate::Registry::default().contains(driver)
    {
        Error::unsupported_reason(
            Capability::DriverOperation,
            format!("driver {driver:?} does not expose an async API"),
        )
    } else {
        Error::DriverFeatureNotEnabled { driver }
    }
}

#[cfg(all(
    test,
    feature = "dummy",
    feature = "hydrasdr",
    any(target_arch = "wasm32", feature = "smol", feature = "tokio")
))]
mod ordering_tests {
    use super::*;

    #[test]
    fn hydrasdr_precedes_the_dummy_fallback() {
        let drivers = AsyncRegistry::default()
            .backends
            .iter()
            .map(RegisteredAsyncDriver::driver)
            .collect::<Vec<_>>();

        assert_eq!(drivers, [Driver::HydraSdr, Driver::Dummy]);
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
