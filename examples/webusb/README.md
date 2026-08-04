# Seify WebUSB example

This is a framework-free Trunk page for exercising Seify's asynchronous WebUSB
API. It opens the first available device from a button click, discovers the
receiver controls exposed by that device, captures a finite block of IQ
samples, and plots their magnitude with the browser's 2D canvas API.

Run it from this directory:

```bash
trunk serve --open
```

`Trunk.toml` selects Cargo's release profile by default.

WebUSB requires a secure context; `localhost` served by Trunk qualifies. Use a
browser with WebUSB support. The first **Open SDR** click requests device
permission.

The application code contains no HydraSDR-specific selection or controls. Its
manifest enables `hydrasdr` because that is currently Seify's only WebUSB
driver. As more asynchronous WASM drivers are added, their features can be
enabled there without changing the page logic.
