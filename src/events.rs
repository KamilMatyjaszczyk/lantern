//! On-demand analysis of raw saved samples. Never called by the collector.
//! Results live only in the viewer; opening history never writes events to disk.
use crate::model::{ProcessUsage, SystemSample};
use std::collections::VecDeque;

pub const MAX_EVENTS: usize = 200;
const RULES: usize = 9;
const MAX_GAP: i64 = 10;
const MIN_DURATION: i64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cpu,
    Memory,
    Swap,
    Temperature,
    Gpu,
    NetworkRx,
    NetworkTx,
    DiskRead,
    DiskWrite,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Cpu => "High CPU",
            Self::Memory => "High memory",
            Self::Swap => "High swap",
            Self::Temperature => "High CPU temperature",
            Self::Gpu => "High GPU",
            Self::NetworkRx => "Network RX surge",
            Self::NetworkTx => "Network TX surge",
            Self::DiskRead => "Disk read surge",
            Self::DiskWrite => "Disk write surge",
        }
    }
    pub fn is_rate(self) -> bool {
        matches!(
            self,
            Self::NetworkRx | Self::NetworkTx | Self::DiskRead | Self::DiskWrite
        )
    }
    fn floor(self) -> f64 {
        match self {
            Self::Cpu | Self::Memory => 90.0,
            Self::Swap => 50.0,
            Self::Temperature => 80.0,
            Self::Gpu => 95.0,
            Self::NetworkRx | Self::NetworkTx => 1_000_000.0,
            Self::DiskRead | Self::DiskWrite => 10_000_000.0,
        }
    }
}
const KINDS: [Kind; RULES] = [
    Kind::Cpu,
    Kind::Memory,
    Kind::Swap,
    Kind::Temperature,
    Kind::Gpu,
    Kind::NetworkRx,
    Kind::NetworkTx,
    Kind::DiskRead,
    Kind::DiskWrite,
];

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub kind: Kind,
    pub start: i64,
    pub end: i64,
    pub peak: f64,
    pub peak_time: i64,
    pub threshold: f64,
    pub process: Option<ProcessUsage>,
    readings: usize,
}

#[derive(Debug, Default)]
pub struct Events {
    /// Most recent first. Bounded even for very noisy history.
    pub items: Vec<Event>,
    pub total: usize,
}

impl Events {
    fn push(&mut self, event: Event, start: i64, end: i64) {
        if event.end < start
            || event.start >= end
            || event.end.saturating_sub(event.start) < MIN_DURATION
            || event.readings < 7
        {
            return;
        }
        self.total += 1;
        self.items.push(event);
        self.items
            .sort_by_key(|e| (std::cmp::Reverse(e.end), std::cmp::Reverse(e.start)));
        self.items.truncate(MAX_EVENTS);
    }
}

#[derive(Default)]
struct Rule {
    pending: Option<Event>,
    baseline: VecDeque<(i64, f64)>,
}

pub struct Analyzer {
    rules: [Rule; RULES],
    events: Events,
    previous: Option<i64>,
    start: i64,
    end: i64,
}

impl Analyzer {
    pub fn new(start: i64, end: i64) -> Self {
        Self {
            rules: std::array::from_fn(|_| Rule::default()),
            events: Events::default(),
            previous: None,
            start,
            end,
        }
    }

    pub fn add(&mut self, sample: &SystemSample) {
        let time = sample.timestamp;
        // Ignore duplicate/reversed timestamps: they cannot prove persistence.
        if self.previous.is_some_and(|previous| time <= previous) {
            return;
        }
        let gap = self
            .previous
            .is_some_and(|previous| time.saturating_sub(previous) > MAX_GAP);
        self.previous = Some(time);
        let values = [
            sample.cpu_percent,
            sample.memory_percent(),
            sample.swap_percent(),
            sample.cpu_temperature,
            sample.gpu_utilisation.map(f64::from),
            sample.network_rx_rate,
            sample.network_tx_rate,
            sample.disk_read_rate,
            sample.disk_write_rate,
        ];
        for (index, kind) in KINDS.into_iter().enumerate() {
            let rule = &mut self.rules[index];
            let value = values[index].filter(|v| v.is_finite() && *v >= 0.0);
            if gap || value.is_none() {
                if let Some(event) = rule.pending.take() {
                    self.events.push(event, self.start, self.end);
                }
                rule.baseline.clear();
            }
            let Some(value) = value else {
                continue;
            };
            while rule
                .baseline
                .front()
                .is_some_and(|(t, _)| time.saturating_sub(*t) > 300)
            {
                rule.baseline.pop_front();
            }
            let threshold = if let Some(event) = &rule.pending {
                Some(event.threshold)
            } else if kind.is_rate() {
                (rule.baseline.len() >= 12
                    && rule
                        .baseline
                        .front()
                        .is_some_and(|(t, _)| time.saturating_sub(*t) >= 55))
                .then(|| {
                    (rule
                        .baseline
                        .iter()
                        .map(|(_, v)| v / rule.baseline.len() as f64)
                        .sum::<f64>()
                        * 6.0)
                        .max(kind.floor())
                })
            } else {
                Some(kind.floor())
            };
            if let Some(threshold) = threshold.filter(|threshold| value >= *threshold) {
                let process = match kind {
                    Kind::Cpu => sample.top_cpu.clone(),
                    Kind::Memory => sample.top_memory.clone(),
                    _ => None,
                };
                if let Some(event) = &mut rule.pending {
                    event.end = time;
                    event.readings += 1;
                    if value > event.peak {
                        event.peak = value;
                        event.peak_time = time;
                        event.process = process;
                    }
                } else {
                    rule.pending = Some(Event {
                        kind,
                        start: time,
                        end: time,
                        peak: value,
                        peak_time: time,
                        threshold,
                        process,
                        readings: 1,
                    });
                }
            } else if let Some(event) = rule.pending.take() {
                self.events.push(event, self.start, self.end);
            }
            if kind.is_rate() {
                rule.baseline.push_back((time, value));
                // Also bound memory for databases with a faster sampling cadence.
                if rule.baseline.len() > 300 {
                    rule.baseline.pop_front();
                }
            }
        }
    }

    pub fn finish(mut self) -> Events {
        for rule in self.rules {
            if let Some(event) = rule.pending {
                self.events.push(event, self.start, self.end);
            }
        }
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cpu(time: i64, value: f64) -> SystemSample {
        let mut s = SystemSample::new(time);
        s.cpu_percent = Some(value);
        s
    }

    #[test]
    fn sustained_episode_merges_and_carries_evidence_from_raw_peak() {
        let mut analyzer = Analyzer::new(0, 100);
        for time in (0..=50).step_by(5) {
            let mut sample = cpu(time, if time == 25 { 99.0 } else { 91.0 });
            if time == 25 {
                sample.top_cpu = Some(ProcessUsage {
                    pid: 42,
                    name: "worker".into(),
                    start_ticks: 1,
                    usage: 80.0,
                });
            }
            analyzer.add(&sample);
        }
        let events = analyzer.finish();
        assert_eq!(events.total, 1);
        let e = &events.items[0];
        assert_eq!((e.start, e.end, e.peak_time), (0, 50, 25));
        assert_eq!(e.peak, 99.0);
        assert_eq!(e.process.as_ref().unwrap().name, "worker");
    }

    #[test]
    fn short_spikes_duplicates_missing_data_and_gaps_do_not_prove_persistence() {
        for times in [
            vec![0, 5, 10],
            vec![0, 0, 0, 30, 30, 30, 30],
            vec![0, 5, 10, 60, 65, 70],
            vec![0, 30, 60, 90, 120, 150, 180],
        ] {
            let mut a = Analyzer::new(0, 200);
            for t in times {
                a.add(&cpu(t, 99.0));
            }
            assert_eq!(a.finish().total, 0);
        }
        let mut a = Analyzer::new(0, 100);
        for t in (0..=50).step_by(5) {
            a.add(&cpu(t, if t == 25 { f64::NAN } else { 99.0 }));
        }
        assert_eq!(a.finish().total, 0);
    }

    #[test]
    fn quiet_data_has_no_events_and_memory_swap_temperature_are_independent() {
        let mut a = Analyzer::new(0, 100);
        for t in (0..=30).step_by(5) {
            a.add(&cpu(t, 30.0));
        }
        assert_eq!(a.finish().total, 0);
        let mut a = Analyzer::new(0, 100);
        for t in (0..=30).step_by(5) {
            let mut s = cpu(t, 0.0);
            s.memory_used = Some(95);
            s.memory_total = Some(100);
            s.swap_used = Some(0);
            s.swap_total = Some(0);
            s.cpu_temperature = Some(81.0);
            a.add(&s);
        }
        let result = a.finish();
        assert_eq!(result.total, 2);
        assert!(result.items.iter().all(|e| e.process.is_none()));
    }

    #[test]
    fn rates_need_a_baseline_and_keep_sustained_surges_as_one_episode() {
        let mut a = Analyzer::new(0, 1000);
        for t in (0..=300).step_by(5) {
            let mut s = SystemSample::new(t);
            s.network_rx_rate = Some(if t < 60 { 100_000.0 } else { 5_000_000.0 });
            a.add(&s);
        }
        let e = a.finish();
        assert_eq!(e.total, 1);
        assert_eq!((e.items[0].start, e.items[0].end), (60, 300));
        let mut a = Analyzer::new(0, 1000);
        for t in (0..=300).step_by(5) {
            let mut s = SystemSample::new(t);
            s.network_rx_rate = Some(5_000_000.0);
            a.add(&s);
        }
        assert_eq!(a.finish().total, 0);
    }

    #[test]
    fn results_are_bounded_recent_first_and_repeated_analysis_is_deterministic() {
        fn analyze() -> Events {
            let mut a = Analyzer::new(0, 100_000);
            for i in 0..300 {
                for t in (0..=35).step_by(5) {
                    a.add(&cpu(i * 40 + t, if t == 35 { 0.0 } else { 99.0 }));
                }
            }
            a.finish()
        }
        let events = analyze();
        assert_eq!(events.total, 300);
        assert_eq!(events.items.len(), MAX_EVENTS);
        assert!(events.items[0].start > events.items[1].start);
        assert_eq!(events.items, analyze().items);
    }

    #[test]
    fn warmup_events_outside_display_window_are_excluded() {
        let mut a = Analyzer::new(100, 200);
        for t in (0..=50).step_by(5) {
            a.add(&cpu(t, 99.0));
        }
        assert_eq!(a.finish().total, 0);
    }
}
