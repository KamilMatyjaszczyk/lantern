use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};

#[derive(Debug, PartialEq, Eq)]
pub struct CpuCounters {
    // user, nice, system, idle, iowait, irq, softirq, steal.
    // guest and guest_nice are already included in user and nice.
    ticks: [u64; 8],
}

impl CpuCounters {
    pub fn parse(stat: &str) -> io::Result<Self> {
        let mut fields = stat
            .lines()
            .find_map(|line| {
                let mut fields = line.split_whitespace();
                (fields.next() == Some("cpu")).then_some(fields)
            })
            .ok_or_else(|| invalid_data("missing aggregate CPU counters"))?;
        let mut ticks = [0; 8];
        for (index, tick) in ticks.iter_mut().enumerate() {
            match fields.next() {
                Some(value) => {
                    *tick = value
                        .parse()
                        .map_err(|_| invalid_data("invalid CPU counter"))?;
                }
                None if index < 4 => return Err(invalid_data("incomplete CPU counters")),
                None => break, // Older kernels may omit the later fields.
            }
        }
        Ok(Self { ticks })
    }

    pub fn total_since(&self, previous: &Self) -> Option<u64> {
        self.ticks
            .iter()
            .zip(&previous.ticks)
            .try_fold(0_u64, |total, (a, b)| total.checked_add(a.checked_sub(*b)?))
            .filter(|total| *total > 0)
    }

    pub fn usage_since(&self, previous: &Self) -> Option<f64> {
        let mut total = 0_u64;
        let mut idle = 0_u64;
        for (index, (&current, &before)) in self.ticks.iter().zip(&previous.ticks).enumerate() {
            // Reboots, CPU hotplug and decreasing iowait invalidate this interval.
            let delta = current.checked_sub(before)?;
            total = total.checked_add(delta)?;
            if index == 3 || index == 4 {
                idle = idle.checked_add(delta)?;
            }
        }
        if total == 0 {
            return None;
        }
        Some((total - idle) as f64 / total as f64 * 100.0)
    }
}

#[derive(Debug, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

impl LoadAverage {
    pub fn parse(loadavg: &str) -> io::Result<Self> {
        let mut fields = loadavg.split_whitespace();
        let mut next_load = || {
            let value = fields
                .next()
                .and_then(|field| field.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(|| invalid_data("missing or invalid load average"))?;
            Ok::<f64, io::Error>(value)
        };
        Ok(Self {
            one: next_load()?,
            five: next_load()?,
            fifteen: next_load()?,
        })
    }
}

pub fn read_counters() -> io::Result<CpuCounters> {
    // Only the aggregate line is needed; do not read individual core counters.
    let mut line = String::new();
    BufReader::new(File::open("/proc/stat")?).read_line(&mut line)?;
    CpuCounters::parse(&line)
}

pub fn read_load_average() -> io::Result<LoadAverage> {
    LoadAverage::parse(&fs::read_to_string("/proc/loadavg")?)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aggregate_and_ignores_guest_double_counting() {
        let counters = CpuCounters::parse(
            "cpu  100 20 30 400 50 6 7 8 90 10\ncpu0 50 10 15 200 25 3 3 4 45 5\n",
        )
        .unwrap();
        assert_eq!(counters.ticks, [100, 20, 30, 400, 50, 6, 7, 8]);
        assert_eq!(
            CpuCounters::parse("cpu 1 2 3 4").unwrap().ticks,
            [1, 2, 3, 4, 0, 0, 0, 0]
        );
    }

    #[test]
    fn rejects_missing_malformed_or_overflowing_counters() {
        for stat in [
            "",
            "cpu0 1 2 3 4",
            "cpu 1 2 3",
            "cpu 1 x 3 4",
            "cpu -1 2 3 4",
            "cpu 18446744073709551616 2 3 4",
        ] {
            assert!(CpuCounters::parse(stat).is_err(), "{stat}");
        }
    }

    #[test]
    fn calculates_usage_from_deltas_with_iowait_as_idle() {
        let before = CpuCounters::parse("cpu 100 20 30 400 50 6 7 8 90 10").unwrap();
        let after = CpuCounters::parse("cpu 120 20 40 450 60 8 10 13 100 10").unwrap();
        // 100 total ticks, 60 idle/iowait ticks, 40 busy ticks.
        assert_eq!(after.usage_since(&before), Some(40.0));
    }

    #[test]
    fn handles_fully_idle_and_fully_busy_intervals() {
        let before = CpuCounters::parse("cpu 10 0 0 10").unwrap();
        assert_eq!(
            CpuCounters::parse("cpu 10 0 0 20")
                .unwrap()
                .usage_since(&before),
            Some(0.0)
        );
        assert_eq!(
            CpuCounters::parse("cpu 20 0 0 10")
                .unwrap()
                .usage_since(&before),
            Some(100.0)
        );
    }

    #[test]
    fn rejects_unchanged_reset_and_decreasing_counters() {
        let before = CpuCounters::parse("cpu 100 20 30 400 50 6 7 8").unwrap();
        assert_eq!(before.usage_since(&before), None);
        assert_eq!(
            CpuCounters::parse("cpu 1 0 0 1")
                .unwrap()
                .usage_since(&before),
            None
        );
        assert_eq!(
            CpuCounters::parse("cpu 120 20 40 450 49 8 10 13")
                .unwrap()
                .usage_since(&before),
            None
        );
        let zero = CpuCounters { ticks: [0; 8] };
        assert_eq!(
            CpuCounters {
                ticks: [u64::MAX; 8]
            }
            .usage_since(&zero),
            None
        );
    }

    #[test]
    fn parses_load_without_treating_it_as_percent() {
        assert_eq!(
            LoadAverage::parse("12.50 3.25 0.00 2/500 123\n").unwrap(),
            LoadAverage {
                one: 12.5,
                five: 3.25,
                fifteen: 0.0
            }
        );
        for value in ["", "1 2", "x 2 3", "-1 2 3", "NaN 2 3", "1 inf 3"] {
            assert!(LoadAverage::parse(value).is_err(), "{value}");
        }
    }
}
