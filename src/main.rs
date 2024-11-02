// industrial-io/examples/riio_bufavg.rs
//
// Simple Rust IIO example for buffered reading and post-processing.
// This does buffered reading with a trigger, then sends the data
// to a second thread to convert and process.
//
// For the sake of simplicity, we assume a raw sample type of a signed,
// 16-bit integer. A real, dedicated application might do something similar,
// but a general-purpose solution would probe the channel type and/or use
// generics to read and convert the raw data.
//
// For quick tests, just set `RawSampleType` to the type matchine the channel
// to be tested.
//
// Copyright (c) 2019, Frank Pagliughi
//
// Licensed under the MIT license:
//   <LICENSE or http://opensource.org/licenses/MIT>
// This file may not be copied, modified, or distributed except according
// to those terms.
//

use anyhow::{bail, Context, Result};
use chrono::offset::Utc;
use chrono::DateTime;

use industrial_io as iio;
use std::{
    any::TypeId,
    cmp, process,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver, SendError, Sender},
        Arc,
    },
    thread::{spawn, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// The type to use for raw samples.
type RawSampleType = i16;

// Time-stamped data buffer
type TsDataBuffer = (u64, Vec<RawSampleType>);

const DFLT_FREQ: i64 = 100;
const DFLT_NUM_SAMPLE: usize = 100;

const SAMPLING_FREQ_ATTR: &str = "sampling_frequency";

/////////////////////////////////////////////////////////////////////////////

// Active data processing object.
// Each one of these has a thread that can process a buffer's worth of
// incoming data at a time.
struct Averager {
    sender: Sender<TsDataBuffer>,
    thr: JoinHandle<()>,
}

impl Averager {
    // Creates a new averager with the specified offset and scale.
    pub fn new(offset: f64, scale: f64) -> Self {
        let (sender, receiver) = channel();
        let thr = spawn(move || Self::thread_func(receiver, offset, scale));
        Self { sender, thr }
    }

    // The internal thread function.
    // This just loops, receiving buffers of data, then averages them,
    // transforming to physical units like Volts, deg C, etc, and then
    // prints them to stdout.
    fn thread_func(receiver: Receiver<TsDataBuffer>, offset: f64, scale: f64) {
        loop {
            let (ts, data): TsDataBuffer = receiver.recv().unwrap();

            if data.is_empty() {
                break;
            }

            // Print the timestamp as the UTC time w/ millisec precision
            let sys_tm = SystemTime::UNIX_EPOCH + Duration::from_nanos(ts);
            let dt: DateTime<Utc> = sys_tm.into();
            print!("{}: ", dt.format("%T%.6f"));

            // Compute the average, then scale the result.
            let sum: f64 = data.iter().map(|&x| f64::from(x)).sum();
            let avg = sum / data.len() as f64;
            let val = (avg + offset) * scale / 1000.0;

            // Print out the scaled average, along with
            // the first few raw values from the buffer
            println!("<{:.2}> - {:?}", val, &data[0..4]);
        }
    }

    // Send data to the thread for processing
    pub fn send(&self, data: TsDataBuffer) -> Result<(), SendError<TsDataBuffer>> {
        self.sender.send(data)
    }

    // Tell the inner thread to quit, then block and wait for it.
    pub fn quit(self) {
        self.sender.send((0, vec![])).unwrap();
        self.thr.join().unwrap();
    }
}

/////////////////////////////////////////////////////////////////////////////

// If the IIO device doesn't have a timestamp channel, we can use this to
// get an equivalent, though less accurate, timestamp.
pub fn timestamp() -> u64 {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Clock error")
        .as_secs_f64();
    (1.0e9 * ts) as u64
}

/////////////////////////////////////////////////////////////////////////////

fn run() -> Result<()> {
    let dev_name = "iio:device0";

    let ctx = iio::Context::new().expect("Couldn't open IIO context.");

    let dev = ctx
        .find_device(dev_name)
        .expect(format!("No IIO device named '{}'", dev_name).as_str());
    println!("channels in dev: {}", dev.num_channels());
    for chan in 0..=1 {
        if let Ok(chan) = dev.get_channel(chan) {
            chan.enable();
        }
    }

    println!("Using device: {}", dev_name);

    // ----- Find the timestamp channel (if any) and a data channel -----
    let ch0 = dev.get_channel(0).unwrap();
    let _ch1 = dev.get_channel(1).unwrap();
    let mut ts_chan = dev.find_channel("timestamp", false);

    if ts_chan.is_some() {
        println!("Found timestamp channel.");
    } else {
        println!("No timestamp channel. Estimating timestamps.");
    }

    println!("Using channel: {}", ch0.name().unwrap_or("??".to_string()));

    // if ch0.type_of() != Some(TypeId::of::<RawSampleType>()) {
    //     bail!(
    //         "The channel type ({:?}) is different than expected.",
    //         ch0.type_of()
    //     );
    // }
    match ch0.type_of() {
        
    }

    if let Some(ref mut chan) = ts_chan {
        chan.enable();
    }

    ch0.enable();

    // ----- Check for a scale and offset -----

    let offset: f64 = ch0.attr_read_float("offset").unwrap_or(0.0);
    let scale: f64 = ch0.attr_read_float("scale").unwrap_or(1.0);

    println!("  Offset: {:.3}, Scale: {:.3}", offset, scale);

    // ----- Set sample frequency and trigger -----

    let freq = DFLT_FREQ;

    // If the user asked for a trigger device, see if we can use it
    if dev.has_attr(SAMPLING_FREQ_ATTR) {
        // Try to set the sampling rate on the device itself, if supported
        dev.attr_write(SAMPLING_FREQ_ATTR, freq).with_context(|| {
            format!(
                "Can't set sampling rate to {}Hz on {}",
                freq,
                dev.name().unwrap()
            )
        })?;
    } else {
        bail!("No suitable trigger device found");
    }

    // ----- Create a buffer -----

    let n_sample = DFLT_NUM_SAMPLE;

    let mut buf = dev
        .create_buffer(n_sample, false)
        .context("Unable to create buffer")?;

    // Make sure the timeout is more than enough to gather each buffer
    // Give 50% extra time, or at least 5sec.
    let ms = cmp::max(5000, 1500 * (n_sample as u64) / (freq as u64));
    if let Err(err) = ctx.set_timeout_ms(ms) {
        eprintln!("Error setting timeout of {}ms: {}", ms, err);
    }

    // ----- Create the averager -----

    let avg = Averager::new(offset, scale);

    // ---- Handle ^C since we want a graceful shutdown -----

    let quit = Arc::new(AtomicBool::new(false));
    // let q = quit.clone();

    // ----- Capture data into the buffer -----

    println!("Started capturing data...");

    while !quit.load(Ordering::SeqCst) {
        buf.refill().context("Error filling the buffer")?;

        // Get the timestamp. Use the time of the _last_ sample.

        let ts: u64 = if let Some(ref chan) = ts_chan {
            buf.channel_iter::<u64>(chan)
                .nth(n_sample - 1)
                .unwrap_or_default()
        } else {
            timestamp()
        };

        // Extract and convert the raw data from the buffer.
        // This puts the raw samples into host format (fixes "endiness" and
        // shifts into place), but it's still raw data. The other thread
        // will apply the offset and scaling.
        // We do this here because the channel is not thread-safe.

        /*
        Note: We could do the following to convert each sample, one at a time,
            but it's more efficient to convert the whole buffer using read()

        let data: Vec<RawSampleType> = buf.channel_iter::<RawSampleType>(&sample_chan)
                                           .map(|x| sample_chan.convert(x))
                                           .collect();
        */

        let data: Vec<RawSampleType> = match ch0.read(&buf) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("Error reading data: {}", err);
                break;
            }
        };

        avg.send((ts, data)).unwrap();
    }

    // ----- Shut down -----

    println!("\nExiting...");
    avg.quit();
    println!("Done");

    Ok(())
}

// --------------------------------------------------------------------------

fn main() {
    if let Err(err) = run() {
        eprintln!("{:#}", err);
        process::exit(1);
    }
}
