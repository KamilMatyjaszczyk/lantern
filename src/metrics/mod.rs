pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod memory;
pub mod network;
pub mod process;
pub mod system;
mod temperature;

use std::{
    io, thread,
    time::{Duration, Instant, SystemTime},
};

use crate::model::{SystemSample, unix_timestamp};

#[derive(Default)]
struct Counters {
    cpu: Option<cpu::CpuCounters>,
    network: Option<(network::NetworkCounters, Instant)>,
    disk: Option<(disk::DiskCounters, Instant)>,
}

impl Counters {
    fn read() -> Self {
        Self {
            cpu: cpu::read_counters().ok(),
            network: network::read_counters()
                .ok()
                .map(|value| (value, Instant::now())),
            disk: disk::read_counters()
                .ok()
                .map(|value| (value, Instant::now())),
        }
    }
}

/// Keep consecutive counters and one NVML session between samples.
pub struct Sampler {
    previous: Counters,
    gpu: Option<gpu::GpuCollector>,
}

impl Sampler {
    /// Initialise the GPU and take a baseline; callers choose when to sample.
    pub fn new() -> Self {
        let gpu = gpu::GpuCollector::new();
        Self {
            previous: Counters::read(),
            gpu,
        }
    }

    /// Read once without sleeping. Rates cover the time since the last reading.
    pub fn sample(&mut self) -> io::Result<SystemSample> {
        let current = Counters::read();
        let mut sample = self.update_counters(current, unix_timestamp(SystemTime::now())?);
        sample.cpu_temperature = temperature::read_cpu_temperature();
        if let Ok(load) = cpu::read_load_average() {
            sample.load_1 = Some(load.one);
            sample.load_5 = Some(load.five);
            sample.load_15 = Some(load.fifteen);
        }
        if let Ok(memory) = memory::read_memory() {
            if let Some(memory) = memory.memory {
                sample.memory_used = Some(memory.used_bytes());
                sample.memory_total = Some(memory.total_bytes);
            }
            if let Some(swap) = memory.swap {
                sample.swap_used = Some(swap.used_bytes());
                sample.swap_total = Some(swap.total_bytes);
            }
        }
        if let Some(gpu) = self.gpu.as_ref().and_then(gpu::GpuCollector::sample) {
            sample.gpu_utilisation = gpu.utilisation;
            sample.gpu_temperature = gpu.temperature;
            if let Some(memory) = gpu.memory {
                sample.gpu_memory_used = Some(memory.used_bytes);
                sample.gpu_memory_total = Some(memory.total_bytes);
            }
        }
        Ok(sample)
    }

    fn update_counters(&mut self, current: Counters, timestamp: i64) -> SystemSample {
        let mut sample = SystemSample::new(timestamp);
        sample.cpu_percent = current
            .cpu
            .as_ref()
            .zip(self.previous.cpu.as_ref())
            .and_then(|(after, before)| after.usage_since(before));
        if let Some(rates) = current
            .network
            .as_ref()
            .zip(self.previous.network.as_ref())
            .and_then(|((after, ended), (before, started))| {
                after.rates_since(before, ended.checked_duration_since(*started)?)
            })
        {
            sample.network_rx_rate = Some(rates.rx_bytes_per_second);
            sample.network_tx_rate = Some(rates.tx_bytes_per_second);
        }
        if let Some(((after, ended), (before, started))) =
            current.network.as_ref().zip(self.previous.network.as_ref())
            && let Some(elapsed) = ended.checked_duration_since(*started)
        {
            sample.network_groups = after.grouped_rates_since(before, elapsed);
        }
        if let Some(rates) = current
            .disk
            .as_ref()
            .zip(self.previous.disk.as_ref())
            .and_then(|((after, ended), (before, started))| {
                after.rates_since(before, ended.checked_duration_since(*started)?)
            })
        {
            sample.disk_read_rate = Some(rates.read_bytes_per_second);
            sample.disk_write_rate = Some(rates.write_bytes_per_second);
        }
        // Even an unavailable/reset reading replaces the baseline. Later
        // samples must never turn a gap into an artificial activity spike.
        self.previous = current;
        sample
    }
}

pub fn collect_snapshot() -> io::Result<SystemSample> {
    let mut sampler = Sampler::new();
    thread::sleep(Duration::from_millis(250));
    sampler.sample()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_counters(stat: &str) -> Counters {
        Counters {
            cpu: Some(cpu::CpuCounters::parse(stat).unwrap()),
            ..Counters::default()
        }
    }

    #[test]
    fn sampler_advances_baseline_for_each_interval() {
        let mut sampler = Sampler {
            previous: cpu_counters("cpu 100 0 0 100"),
            gpu: None,
        };
        let first = sampler.update_counters(cpu_counters("cpu 120 0 0 180"), 10);
        let second = sampler.update_counters(cpu_counters("cpu 200 0 0 200"), 15);
        assert_eq!(first.cpu_percent, Some(20.0));
        assert_eq!(second.cpu_percent, Some(80.0));
        assert_eq!(second.timestamp, 15);
    }

    #[test]
    fn sampler_recovers_after_missing_readings_and_counter_resets() {
        let mut sampler = Sampler {
            previous: cpu_counters("cpu 100 0 0 100"),
            gpu: None,
        };
        assert_eq!(
            sampler.update_counters(Counters::default(), 10).cpu_percent,
            None
        );
        assert_eq!(
            sampler
                .update_counters(cpu_counters("cpu 200 0 0 200"), 15)
                .cpu_percent,
            None
        );
        assert_eq!(
            sampler
                .update_counters(cpu_counters("cpu 1 0 0 1"), 20)
                .cpu_percent,
            None
        );
        assert_eq!(
            sampler
                .update_counters(cpu_counters("cpu 2 0 0 2"), 25)
                .cpu_percent,
            Some(50.0)
        );
    }

    #[test]
    fn sampler_network_rates_cover_each_full_interval() {
        fn network_counters(rx: u64, tx: u64, time: Instant) -> Counters {
            let text = format!(
                "Inter-| Receive | Transmit\nface |bytes |bytes\neth0: {rx} 0 0 0 0 0 0 0 {tx} 0 0 0 0 0 0 0\n"
            );
            Counters {
                network: Some((network::NetworkCounters::parse(&text).unwrap(), time)),
                ..Counters::default()
            }
        }
        let start = Instant::now();
        let mut sampler = Sampler {
            previous: network_counters(100, 200, start),
            gpu: None,
        };
        let first = sampler.update_counters(
            network_counters(600, 1200, start + Duration::from_secs(5)),
            10,
        );
        let second = sampler.update_counters(
            network_counters(800, 1600, start + Duration::from_secs(7)),
            12,
        );
        assert_eq!(first.network_rx_rate, Some(100.0));
        assert_eq!(first.network_tx_rate, Some(200.0));
        assert_eq!(second.network_rx_rate, Some(100.0));
        assert_eq!(second.network_tx_rate, Some(200.0));
    }
}
