//! Shared data contract for current output, history, and future interfaces.
//!
//! Byte counts are bytes, rates are bytes/second, temperatures are Celsius,
//! and utilisation is 0–100 percent. Load averages are not percentages.
//! `None` means unavailable; `Some(0)` is a measured zero.

use std::{
    io,
    time::{SystemTime, UNIX_EPOCH},
};

/// Current machine metadata; historical resource samples do not store it.
#[derive(Debug, Default)]
pub struct MachineInfo {
    pub hostname: Option<String>,
    pub uptime_seconds: Option<u64>,
}

/// Largest observed process user at a sample, not proof of causality.
/// `usage` is percent of the whole machine for CPU, bytes of RSS for memory.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessUsage {
    pub pid: u32,
    pub name: String,
    pub start_ticks: u64,
    pub usage: f64,
}

/// Group order: Ethernet, Wi-Fi, virtual, unknown. Within each group:
/// RX bytes/s, TX bytes/s, RX errors/s, TX errors/s, RX drops/s, TX drops/s.
/// Missing groups and invalid/reset counters are None, never guessed zero.
pub type NetworkGroups = [[Option<f64>; 6]; 4];
pub const NETWORK_GROUP_NAMES: [&str; 4] = ["Ethernet", "Wi-Fi", "Virtual", "Unknown"];

#[derive(Debug, Clone, PartialEq)]
pub struct SystemSample {
    /// Whole seconds since the Unix epoch (UTC), rounded down.
    pub timestamp: i64,
    pub top_cpu: Option<ProcessUsage>,
    pub top_memory: Option<ProcessUsage>,
    pub cpu_percent: Option<f64>,
    pub load_1: Option<f64>,
    pub load_5: Option<f64>,
    pub load_15: Option<f64>,
    pub memory_used: Option<u64>,
    pub memory_total: Option<u64>,
    pub swap_used: Option<u64>,
    pub swap_total: Option<u64>,
    pub network_groups: NetworkGroups,
    pub network_rx_rate: Option<f64>,
    pub network_tx_rate: Option<f64>,
    pub disk_read_rate: Option<f64>,
    pub disk_write_rate: Option<f64>,
    pub cpu_temperature: Option<f64>,
    pub gpu_utilisation: Option<u32>,
    pub gpu_memory_used: Option<u64>,
    pub gpu_memory_total: Option<u64>,
    pub gpu_temperature: Option<u32>,
}

impl SystemSample {
    /// Start with a known timestamp and no measurements, never fake zeroes.
    pub fn new(timestamp: i64) -> Self {
        Self {
            timestamp,
            top_cpu: None,
            top_memory: None,
            cpu_percent: None,
            load_1: None,
            load_5: None,
            load_15: None,
            memory_used: None,
            memory_total: None,
            swap_used: None,
            swap_total: None,
            network_groups: [[None; 6]; 4],
            network_rx_rate: None,
            network_tx_rate: None,
            disk_read_rate: None,
            disk_write_rate: None,
            cpu_temperature: None,
            gpu_utilisation: None,
            gpu_memory_used: None,
            gpu_memory_total: None,
            gpu_temperature: None,
        }
    }

    pub fn memory_percent(&self) -> Option<f64> {
        usage_percent(self.memory_used, self.memory_total)
    }

    pub fn swap_percent(&self) -> Option<f64> {
        usage_percent(self.swap_used, self.swap_total)
    }

    pub fn has_gpu_data(&self) -> bool {
        self.gpu_utilisation.is_some()
            || self.gpu_memory_used.is_some()
            || self.gpu_memory_total.is_some()
            || self.gpu_temperature.is_some()
    }
}

/// Derive percentages from the stored byte counts so interfaces agree.
fn usage_percent(used: Option<u64>, total: Option<u64>) -> Option<f64> {
    let (used, total) = used.zip(total)?;
    (total > 0 && used <= total).then(|| used as f64 / total as f64 * 100.0)
}

pub fn unix_timestamp(time: SystemTime) -> io::Result<i64> {
    let seconds = match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::from(duration.as_secs()),
        Err(error) => {
            let duration = error.duration();
            -i128::from(duration.as_secs()) - i128::from(duration.subsec_nanos() > 0)
        }
    };
    i64::try_from(seconds)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "timestamp is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamp_uses_epoch_seconds_and_floors_subseconds() {
        assert_eq!(unix_timestamp(UNIX_EPOCH).unwrap(), 0);
        assert_eq!(
            unix_timestamp(UNIX_EPOCH + Duration::from_millis(1234567)).unwrap(),
            1234
        );
        assert_eq!(
            unix_timestamp(UNIX_EPOCH - Duration::from_millis(1)).unwrap(),
            -1
        );
        assert_eq!(
            unix_timestamp(UNIX_EPOCH - Duration::from_secs(1)).unwrap(),
            -1
        );
    }

    #[test]
    fn unavailable_metrics_are_distinct_from_measured_zeroes() {
        let mut sample = SystemSample::new(1234);
        assert_eq!(sample.timestamp, 1234);
        assert_eq!(sample.cpu_percent, None);
        assert_eq!(sample.memory_percent(), None);
        assert_eq!(sample.swap_percent(), None);
        assert!(!sample.has_gpu_data());
        sample.cpu_percent = Some(0.0);
        sample.gpu_utilisation = Some(0);
        assert_eq!(sample.cpu_percent, Some(0.0));
        assert!(sample.has_gpu_data());
    }

    #[test]
    fn percentages_are_derived_from_bytes() {
        let mut sample = SystemSample::new(1234);
        sample.memory_used = Some(5 * 1024_u64.pow(3));
        sample.memory_total = Some(8 * 1024_u64.pow(3));
        sample.swap_used = Some(1536 * 1024);
        sample.swap_total = Some(2048 * 1024);
        assert_eq!(sample.memory_percent(), Some(62.5));
        assert_eq!(sample.swap_percent(), Some(75.0));
    }

    #[test]
    fn percentages_handle_zero_partial_and_invalid_data() {
        for (used, total, expected) in [
            (None, Some(100), None),
            (Some(1), None, None),
            (Some(0), Some(0), None),
            (Some(101), Some(100), None),
            (Some(0), Some(100), Some(0.0)),
            (Some(100), Some(100), Some(100.0)),
        ] {
            assert_eq!(usage_percent(used, total), expected);
        }
    }

    #[test]
    fn partial_gpu_readings_remain_visible() {
        let mut sample = SystemSample::new(1234);
        sample.gpu_temperature = Some(44);
        assert!(sample.has_gpu_data());
        assert_eq!(sample.gpu_utilisation, None);
        assert_eq!(sample.gpu_memory_used, None);
    }
}
