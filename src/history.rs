//! Streaming, UTC-based aggregation, independent of terminal presentation.

use crate::model::SystemSample;

#[derive(Clone, Copy, Debug, Default)]
pub struct Statistics {
    count: u64,
    mean: f64,
    peak: Option<(f64, i64)>,
    timestamps: Option<(i64, i64)>,
}

impl Statistics {
    fn add(&mut self, value: f64, timestamp: i64) {
        self.count += 1;
        self.timestamps = Some(
            self.timestamps
                .map_or((timestamp, timestamp), |(first, last)| {
                    (first.min(timestamp), last.max(timestamp))
                }),
        );
        // Incremental mean avoids overflowing a sum of large byte rates.
        self.mean += (value - self.mean) / self.count as f64;
        if self
            .peak
            .is_none_or(|(peak, time)| value > peak || (value == peak && timestamp < time))
        {
            self.peak = Some((value, timestamp));
        }
    }

    pub fn mean(&self) -> Option<f64> {
        (self.count > 0).then_some(self.mean)
    }

    pub fn peak(&self) -> Option<(f64, i64)> {
        self.peak
    }

    /// First and last valid measurements in this aggregate (UTC seconds).
    pub fn timestamps(&self) -> Option<(i64, i64)> {
        self.timestamps
    }
}

/// Approximate bytes over consecutive observed intervals, not over gaps.
#[derive(Debug, Default)]
pub struct TransferEstimate {
    bytes: f64,
    pub seconds: u64,
}

impl TransferEstimate {
    pub fn bytes(&self) -> Option<f64> {
        (self.seconds > 0 && self.bytes.is_finite()).then_some(self.bytes)
    }
    fn add(&mut self, rate: Option<f64>, seconds: u64) {
        if seconds > 0
            && let Some(rate) = rate.filter(|v| v.is_finite() && *v >= 0.0)
        {
            self.bytes += rate * seconds as f64;
            self.seconds += seconds;
        }
    }
}

#[derive(Debug)]
pub struct MemoryReading {
    pub timestamp: i64,
    pub ram: Option<(u64, u64)>,
    pub swap: Option<(u64, u64)>,
    pub vram: Option<(u64, u64)>,
}

#[derive(Debug)]
pub struct Series {
    pub buckets: Vec<Statistics>,
    pub summary: Statistics,
    pub transfer: TransferEstimate,
}

impl Series {
    fn new(columns: usize) -> Self {
        Self {
            buckets: vec![Statistics::default(); columns],
            summary: Statistics::default(),
            transfer: TransferEstimate::default(),
        }
    }

    fn add(&mut self, column: usize, value: Option<f64>, timestamp: i64) {
        if let Some(value) = value.filter(|value| value.is_finite() && *value >= 0.0) {
            self.buckets[column].add(value, timestamp);
            self.summary.add(value, timestamp);
        }
    }
}

/// CPU %, CPU temperature °C, memory %, network RX/TX bytes/s, GPU %,
/// swap %, disk read/write bytes/s, GPU temperature °C, VRAM %.
/// Memory use is bounded by the requested column count, not the number of rows.
pub struct History {
    pub start: i64,
    pub end: i64,
    pub rows: u64,
    pub series: [Series; 11],
    pub network_groups: [[Statistics; 6]; 4],
    pub last_memory: Option<MemoryReading>,
    previous_timestamp: Option<i64>,
}

impl History {
    /// Half-open UTC range [start, end), with 1..=512 equal-duration buckets.
    pub fn new(start: i64, end: i64, columns: usize) -> Option<Self> {
        if end <= start || !(1..=512).contains(&columns) {
            return None;
        }
        Some(Self {
            start,
            end,
            rows: 0,
            network_groups: [[Statistics::default(); 6]; 4],
            last_memory: None,
            previous_timestamp: None,
            series: std::array::from_fn(|_| Series::new(columns)),
        })
    }

    pub fn add(&mut self, sample: &SystemSample) {
        if sample.timestamp >= self.end {
            return;
        }
        let previous = self.previous_timestamp;
        if previous.is_none_or(|time| sample.timestamp > time) {
            self.previous_timestamp = Some(sample.timestamp);
        }
        if sample.timestamp < self.start {
            return;
        }
        // Only normal adjacent samples contribute. A missing five-second sample
        // must not make the next rate cover a ten-second gap. Whole timestamps
        // can differ by 4–6 seconds due to collection timing and rounding.
        let seconds = previous
            .and_then(|time| {
                let gap = sample.timestamp.checked_sub(time)?;
                (1..=6)
                    .contains(&gap)
                    .then(|| sample.timestamp.saturating_sub(time.max(self.start)) as u64)
            })
            .unwrap_or(0);
        if self
            .last_memory
            .as_ref()
            .is_none_or(|last| sample.timestamp >= last.timestamp)
        {
            let valid = |used: Option<u64>, total: Option<u64>| {
                used.zip(total).filter(|(used, total)| used <= total)
            };
            self.last_memory = Some(MemoryReading {
                timestamp: sample.timestamp,
                ram: valid(sample.memory_used, sample.memory_total).filter(|(_, total)| *total > 0),
                swap: valid(sample.swap_used, sample.swap_total),
                vram: valid(sample.gpu_memory_used, sample.gpu_memory_total)
                    .filter(|(_, total)| *total > 0),
            });
        }
        // Widen before subtraction/multiplication, including ranges crossing epoch.
        let offset = i128::from(sample.timestamp) - i128::from(self.start);
        let span = i128::from(self.end) - i128::from(self.start);
        let column = (offset * self.series[0].buckets.len() as i128 / span) as usize;
        self.rows += 1;
        let percent = |value: Option<f64>| value.filter(|value| *value <= 100.0);
        let values = [
            percent(sample.cpu_percent),
            sample.cpu_temperature,
            sample.memory_percent(),
            sample.network_rx_rate,
            sample.network_tx_rate,
            percent(sample.gpu_utilisation.map(f64::from)),
            sample.swap_percent(),
            sample.disk_read_rate,
            sample.disk_write_rate,
            sample.gpu_temperature.filter(|v| *v <= 250).map(f64::from),
            sample
                .gpu_memory_used
                .zip(sample.gpu_memory_total)
                .filter(|(u, t)| *t > 0 && u <= t)
                .map(|(u, t)| u as f64 / t as f64 * 100.0),
        ];
        for (stats, readings) in self.network_groups.iter_mut().zip(sample.network_groups) {
            for (stat, value) in stats.iter_mut().zip(readings) {
                if let Some(value) = value.filter(|v| v.is_finite() && *v >= 0.0) {
                    stat.add(value, sample.timestamp);
                }
            }
        }
        for (index, (series, value)) in self.series.iter_mut().zip(values).enumerate() {
            series.add(column, value, sample.timestamp);
            if matches!(index, 3 | 4 | 7 | 8) {
                series.transfer.add(value, seconds);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu(timestamp: i64, value: Option<f64>) -> SystemSample {
        let mut sample = SystemSample::new(timestamp);
        sample.cpu_percent = value;
        sample
    }

    #[test]
    fn boundaries_gaps_and_zeroes_remain_distinct() {
        let mut history = History::new(10, 50, 4).unwrap();
        for (time, value) in [
            (9, 99.0),
            (10, 0.0),
            (19, 20.0),
            (20, 30.0),
            (49, 40.0),
            (50, 99.0),
        ] {
            history.add(&cpu(time, Some(value)));
        }
        assert_eq!(history.rows, 4);
        let series = &history.series[0];
        assert_eq!(
            series
                .buckets
                .iter()
                .map(Statistics::mean)
                .collect::<Vec<_>>(),
            vec![Some(10.0), Some(30.0), None, Some(40.0)]
        );
        assert_eq!(series.summary.mean(), Some(22.5));
        assert_eq!(series.summary.peak(), Some((40.0, 49)));
    }

    #[test]
    fn summary_weights_samples_and_preserves_raw_peak_and_earliest_tie() {
        let mut history = History::new(0, 100, 2).unwrap();
        for (time, value) in [(90, 90.0), (3, 0.0), (1, 0.0), (2, 90.0)] {
            history.add(&cpu(time, Some(value)));
        }
        assert_eq!(history.series[0].summary.mean(), Some(45.0));
        assert_eq!(history.series[0].summary.peak(), Some((90.0, 2)));
        assert_eq!(history.series[0].buckets[0].mean(), Some(30.0));
    }

    #[test]
    fn unavailable_and_invalid_metrics_do_not_become_zeroes() {
        let mut history = History::new(0, 10, 1).unwrap();
        for value in [
            None,
            Some(f64::NAN),
            Some(f64::INFINITY),
            Some(-1.0),
            Some(101.0),
        ] {
            history.add(&cpu(1, value));
        }
        for series in &history.series {
            assert_eq!(series.summary.mean(), None);
        }
        let mut sample = cpu(2, Some(0.0));
        sample.memory_used = Some(25);
        sample.memory_total = Some(100);
        sample.network_rx_rate = Some(1000.0);
        sample.network_tx_rate = Some(0.0);
        sample.gpu_utilisation = Some(50);
        history.add(&sample);
        assert_eq!(
            history
                .series
                .each_ref()
                .map(|series| series.summary.mean()),
            [
                Some(0.0),
                None,
                Some(25.0),
                Some(1000.0),
                Some(0.0),
                Some(50.0),
                None,
                None,
                None,
                None,
                None
            ]
        );
    }

    #[test]
    fn transfer_estimates_exclude_gaps_duplicates_missing_values_and_outside_history() {
        let mut history = History::new(0, 40, 8).unwrap();
        for (time, rate) in [
            (-5, Some(100.0)),
            (0, Some(100.0)),
            (5, Some(100.0)),
            (5, Some(999.0)),
            (15, Some(100.0)),
            (20, None),
            (25, Some(100.0)),
            (30, Some(0.0)),
            (35, Some(f64::INFINITY)),
            (40, Some(100.0)),
        ] {
            let mut sample = SystemSample::new(time);
            sample.network_rx_rate = rate;
            history.add(&sample);
        }
        // [0,5], [20,25], and measured zero [25,30]. No guessed missing bytes.
        assert_eq!(history.series[3].transfer.bytes(), Some(1000.0));
        assert_eq!(history.series[3].transfer.seconds, 15);
        assert_eq!(history.series[4].transfer.bytes(), None);
        let mut empty = History::new(0, 20, 8).unwrap();
        let mut sample = SystemSample::new(5);
        sample.network_rx_rate = Some(100.0);
        empty.add(&sample);
        assert_eq!(empty.series[3].transfer.bytes(), None);
    }

    #[test]
    fn transfer_estimates_clip_first_interval_and_do_not_restart_after_overflow() {
        let mut history = History::new(2, 20, 8).unwrap();
        for time in [0, 5, 10] {
            let mut s = SystemSample::new(time);
            s.disk_read_rate = Some(100.0);
            history.add(&s);
        }
        assert_eq!(history.series[7].transfer.bytes(), Some(800.0));
        assert_eq!(history.series[7].transfer.seconds, 8);
        let mut transfer = TransferEstimate::default();
        transfer.add(Some(f64::MAX), 5);
        transfer.add(Some(10.0), 5);
        assert_eq!(transfer.bytes(), None);
    }

    #[test]
    fn latest_memory_values_do_not_reuse_stale_data_or_confuse_disabled_swap() {
        let mut h = History::new(0, 100, 8).unwrap();
        let mut sample = SystemSample::new(10);
        sample.memory_used = Some(2);
        sample.memory_total = Some(4);
        sample.swap_used = Some(0);
        sample.swap_total = Some(0);
        h.add(&sample);
        let last = h.last_memory.as_ref().unwrap();
        assert_eq!(last.ram, Some((2, 4)));
        assert_eq!(last.swap, Some((0, 0)));
        h.add(&SystemSample::new(15));
        assert!(h.last_memory.as_ref().unwrap().ram.is_none());
        h.add(&sample);
        assert_eq!(h.last_memory.as_ref().unwrap().timestamp, 15);
    }

    #[test]
    fn invalid_dimensions_and_extreme_timestamps_are_safe() {
        for (start, end, columns) in [(0, 0, 1), (1, 0, 1), (0, 1, 0), (0, 1, 513)] {
            assert!(History::new(start, end, columns).is_none());
        }
        let mut history = History::new(i64::MIN, i64::MAX, 2).unwrap();
        history.add(&cpu(i64::MIN, Some(1.0)));
        history.add(&cpu(i64::MAX - 1, Some(3.0)));
        assert_eq!(history.series[0].summary.mean(), Some(2.0));
        assert_eq!(history.series[0].buckets[1].mean(), Some(3.0));
    }
}
