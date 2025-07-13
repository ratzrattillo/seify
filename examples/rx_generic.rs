use clap::Parser;
use num_complex::Complex32;

use seify::Device;
use seify::Direction::Rx;
use seify::RxStreamer;

#[derive(Parser, Debug)]
#[clap(version)]
struct Args {
    /// Device Filters
    #[clap(short, long, default_value = "")]
    args: String,
}

#[tokio::main]
pub async fn main() -> Result<(), Box<dyn std::error::Error>> {
    //env_logger::init();
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .filter_module("nusb", log::LevelFilter::Info)
        .init();
    let cli = Args::parse();

    let dev = Device::from_args(cli.args)?;
    // Get typed reference to device impl
    // let r: &seify::impls::RtlSdr = dev.impl_ref().unwrap();

    // HackRf doesnt support agc
    if dev.supports_agc(Rx, 0)? {
        dev.enable_agc(Rx, 0, true)?;
    }
    println!("enabled agc!");
    // TODO For bladerf enable expansion board

    dev.set_frequency(Rx, 0, 927e6)?;
    println!("set frequency!");
    dev.set_sample_rate(Rx, 0, 3.2e6)?;
    println!("set sample rate");

    println!("driver:      {:?}", dev.driver());
    println!("id:          {:?}", dev.id()?);
    println!("info:        {:?}", dev.info()?);
    println!("sample rate: {:?}", dev.sample_rate(Rx, 0)?);
    println!("frequency:   {:?}", dev.frequency(Rx, 0)?);
    println!("gain:        {:?}", dev.gain(Rx, 0)?);

    // buffers contains one destination slice for each channel of this stream.
    let mut rx_channel0_samps = [Complex32::new(0.0, 0.0); 8192];
    // let mut rx_channel1_samps = [Complex32::new(0.0, 0.0); 8192];
    // let mut rx_samps = [rx_channel0_samps.as_mut_slice(), rx_channel1_samps.as_mut_slice()];
    let mut rx_samps = [rx_channel0_samps.as_mut_slice()];
    let mut rx = dev.rx_streamer(&[0])?;
    println!("obtained rx streamer");
    rx.activate()?;
    println!("rx activation");
    // let n = rx.read(&mut [&mut rx_samps], 200000)?;
    let n = rx.read(&mut rx_samps, 200000)?;
    println!("rx read");

    // plot(&mut rx_samps[..n]);
    plot(&mut rx_samps[0][..n]);

    Ok(())
}

fn plot(s: &mut [num_complex::Complex32]) {
    use gnuplot::*;

    let mut planner = rustfft::FftPlanner::new();
    planner.plan_fft_forward(s.len()).process(s);

    let abs: Vec<f32> = s.iter().map(|s| s.norm_sqr().log10()).collect();

    let mut fg = Figure::new();
    fg.axes2d().set_title("Spectrum", &[]).lines(
        0..s.len(),
        abs,
        &[LineWidth(3.0), Color("blue"), LineStyle(DotDash)],
    );
    fg.show().unwrap();
}
