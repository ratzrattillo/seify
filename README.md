# Seify

Rust SDR hardware abstraction for applications that want one API over multiple
radio backends.

## What Seify Provides

A clear path towards a great Rust SDR driver ecosystem.

- One API for probing, opening, configuring, and streaming from SDR devices.
- Typed devices when an application wants a concrete backend.
- Type-erased devices when an application wants runtime driver selection.
- Capability-oriented channel APIs, so backends expose the controls they support.
- Feature-gated drivers, so each binary only includes the SDR backends it needs.
- SoapySDR support for broad hardware coverage and native Rust drivers where available.

The native Rust drivers are still experimental. For production use and the
widest set of stable hardware integrations, prefer the SoapySDR backend.

## Features

The default feature set is `soapy`.

Enable drivers explicitly in `Cargo.toml` or on the command line:

```bash
cargo check --no-default-features --features rtlsdr
cargo check --no-default-features --features hydrasdr,hackrfone
```

Available features:

| Feature | Driver argument | Notes |
| --- | --- | --- |
| `dummy` | `driver=dummy` | Driver for unit tests. |
| `soapy` | `driver=soapy` | SoapySDR backend. Enabled by default. Requires SoapySDR system libraries. |
| `aaronia_http` | `driver=aaronia_http` | Aaronia HTTP backend. |
| `bladerf1` | `driver=bladerf` | bladeRF 1 backend. |
| `hackrfone` | `driver=hackrfone` | HackRF One backend. |
| `hydrasdr` | `driver=hydrasdr` | HydraSDR backend; async WebUSB support on `wasm32-unknown-unknown`. |
| `rtlsdr` | `driver=rtlsdr` | RTL-SDR backend. |
| `smol` / `tokio` | n/a | Pick one for async `nusb` runtime integration. |

For native async use with `nusb`-based drivers, enable exactly one of `smol` or
`tokio`. For example, native HydraSDR async support is enabled with
`hydrasdr,smol` or `hydrasdr,tokio`. WebAssembly uses WebUSB and needs only the
`hydrasdr` feature.

## WebUSB

HydraSDR is the first Seify hardware driver available on
`wasm32-unknown-unknown`. Only `AsyncHydraSdr`, `AsyncRegistry`, and the async
device/streamer APIs are connected to that driver on wasm; the synchronous
HydraSDR backend remains native-only.

Build it with:

```bash
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrasdr
```

WebUSB's `web-sys` bindings require `--cfg=web_sys_unstable_apis`; this
repository supplies it for `wasm32-unknown-unknown` in `.cargo/config.toml`.
Applications consuming Seify as a dependency must add the same target setting
to their own Cargo configuration. A browser only probes HydraSDRs already
authorized for the page. Opening the async HydraSDR backend requests permission
when necessary, so the first open must run from a browser user gesture.

A framework-free, driver-agnostic browser example with discovered receiver
controls and a finite-capture magnitude plot lives in `examples/webusb`. Run it
with:

```bash
cd examples/webusb
trunk serve --open
```

Use the generic API with an argument string to select a backend at runtime:

```bash
cargo run --no-default-features --features rtlsdr --example probe -- --args driver=rtlsdr
cargo run --no-default-features --features rtlsdr --example rx_generic -- --args driver=rtlsdr
```

Additional driver-specific arguments can be passed in the same string:

```bash
cargo run --no-default-features --features soapy --example probe -- --args driver=soapy,soapy_driver=rtlsdr
```

## Example

```rust
use num_complex::Complex32;
use seify::DynDevice;

pub fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dev = DynDevice::new()?;
    let rx0 = dev.rx(0)?;
    let mut samps = [Complex32::new(0.0, 0.0); 1024];
    let mut rx = rx0.streamer()?;
    rx.activate()?;
    let n = rx.read(&mut [&mut samps], 200000)?;
    println!("read {n} samples");

    Ok(())
}
```
