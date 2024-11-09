use industrial_io as iio;
use std::{process, time::Duration};

const DFLT_DEV_NAME: &str = "iio:device0";
const SAMPLETIME: Duration = Duration::from_millis(1000);

fn main() {
    let dev_name = DFLT_DEV_NAME;

    let ctx = iio::Context::new().unwrap_or_else(|_err| {
        println!("Couldn't open IIO context.");
        process::exit(1);
    });

    let dev = ctx.find_device(dev_name).unwrap_or_else(|| {
        println!("No IIO device named '{}'", dev_name);
        process::exit(2);
    });

    println!("channels in dev: {}", dev.num_channels());
    let ch0 = dev.get_channel(0).unwrap();
    ch0.enable();
    let mut buf = dev.create_buffer(512, false).unwrap_or_else(|err| {
        eprintln!("Unable to create buffer: {}", err);
        process::exit(3);
    });

    let mut avgs = Vec::new();
    let mut start = std::time::Instant::now();
    let mut accum = 0.0;
    loop {
        let mut max = 0;
        let mut min = 4095;
        while start + SAMPLETIME > std::time::Instant::now() {
            if let Err(err) = buf.refill() {
                eprintln!("Error filling the buffer: {}", err);
                process::exit(4);
            }
            let data = buf.channel_iter::<u16>(&ch0).collect::<Vec<u16>>();
            let avg = data.iter().fold(0, |acc: u64, x| acc + *x as u64) / data.len() as u64;
            min = *std::cmp::min(data.iter().min().unwrap(), &min);
            max = *std::cmp::max(data.iter().max().unwrap(), &max);
            avgs.push(avg);
        }
        start += SAMPLETIME;
        let maxcurrent = adc2current(max as u64);
        let mincurrent = adc2current(min as u64);
        let avgcurrent = adc2current(avgs.iter().sum::<u64>() / avgs.len() as u64);
        accum += (avgcurrent as f64) / 3600.0 / 1000.0;
        println!(
            "{:.2} mAh, avg: {:.2} mA, max: {:.2} mA, min: {:.2} mA",
            accum,
            avgcurrent as f32 / 1000.0,
            maxcurrent as f32 / 1000.0,
            mincurrent as f32 / 1000.0
        );
        avgs.clear();
    }
}

// ZXCT1009 amp = 10mA = 104uA over 10K resistor = 1040mV
fn adc2current(adc: u64) -> u64 {
    let vref = 1800;
    let adc = adc as u64 * 1000;
    let adc_max = 4095;
    let current = (adc * vref / adc_max) / 104;
    current
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_adc2current() {
        assert_eq!(adc2current(0), 0);
        assert_eq!(adc2current(2366), 10_000); // 10mA
        assert_eq!(adc2current(4095), 17_307); // Max
    }
    #[test]
    fn test_currentEstimate() {
        let mut accum = 0.0;
        let mut avgs = vec![2366, 2366, 2366, 2366, 2366];
        let avgcurrent = adc2current(avgs.iter().sum::<u64>() / avgs.len() as u64);
        accum += (avgcurrent as f64) / 3600.0 / 1000.0;
        assert_eq!(accum, 0.0005);
    }
}
