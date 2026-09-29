//! Small historical evidence, not event detection. One stat read per visible PID.
//! Field definitions: https://docs.kernel.org/filesystems/proc.html
use std::{collections::HashMap, fs};

use super::cpu::{self, CpuCounters};
use crate::model::{ProcessUsage, SystemSample};

struct Stat {
    pid: u32,
    name: String,
    start: u64,
    ticks: u64,
    rss: u64,
}

fn parse(text: &str, page_size: u64) -> Option<Stat> {
    // comm may contain spaces and parentheses, including ')'.
    let (pid, tail) = text.split_once('(')?;
    let (name, fields) = tail.rsplit_once(')')?;
    let fields: Vec<_> = fields.split_whitespace().take(22).collect();
    let number = |index: usize| fields.get(index)?.parse::<u64>().ok();
    Some(Stat {
        pid: pid.trim().parse().ok().filter(|pid| *pid > 0)?,
        // Keep stored names safe and narrow in the terminal. No command arguments.
        name: name
            .chars()
            .take(32)
            .map(|c| {
                if c.is_ascii() && !c.is_control() {
                    c
                } else {
                    '?'
                }
            })
            .collect(),
        ticks: number(11)?.checked_add(number(12)?)?, // utime + stime
        start: number(19)?,
        rss: number(21)?.checked_mul(page_size)?,
    })
}

fn cpu_usage(stat: &Stat, previous: Option<&(u64, u64)>, total: Option<u64>) -> Option<f64> {
    let &(start, ticks) = previous?;
    if start != stat.start {
        return None;
    } // PID reuse
    let total = total.filter(|total| *total > 0)?;
    let delta = stat.ticks.checked_sub(ticks)?;
    let usage = delta as f64 / total as f64 * 100.0;
    // Sequential /proc reads are not atomic. Reject inconsistent intervals.
    (usage > 0.0 && usage <= 100.0).then_some(usage)
}

pub struct ProcessSampler {
    previous: HashMap<u32, (u64, u64)>,
    cpu: Option<CpuCounters>,
    page_size: Option<u64>,
}

impl ProcessSampler {
    pub fn new() -> Self {
        // SAFETY: sysconf takes a constant and accesses no Rust memory.
        let page_size = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
            .ok()
            .filter(|size| *size > 0);
        let mut sampler = Self {
            previous: HashMap::new(),
            cpu: None,
            page_size,
        };
        sampler.sample(&mut SystemSample::new(0));
        sampler
    }

    pub fn sample(&mut self, sample: &mut SystemSample) {
        let current_cpu = cpu::read_counters().ok();
        let total = current_cpu
            .as_ref()
            .zip(self.cpu.as_ref())
            .and_then(|(a, b)| a.total_since(b));
        self.cpu = current_cpu;
        let mut current = HashMap::with_capacity(self.previous.len());
        if let (Ok(entries), Some(page_size)) = (fs::read_dir("/proc"), self.page_size) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.parse::<u32>().ok())
                    .is_none()
                {
                    continue;
                }
                let Ok(text) = fs::read_to_string(entry.path().join("stat")) else {
                    continue;
                };
                let Some(stat) = parse(&text, page_size) else {
                    continue;
                };
                if let Some(usage) = cpu_usage(&stat, self.previous.get(&stat.pid), total)
                    && sample.top_cpu.as_ref().is_none_or(|p| usage > p.usage)
                {
                    sample.top_cpu = Some(ProcessUsage {
                        pid: stat.pid,
                        name: stat.name.clone(),
                        start_ticks: stat.start,
                        usage,
                    });
                }
                if stat.rss > 0
                    && sample
                        .top_memory
                        .as_ref()
                        .is_none_or(|p| stat.rss as f64 > p.usage)
                {
                    sample.top_memory = Some(ProcessUsage {
                        pid: stat.pid,
                        name: stat.name,
                        start_ticks: stat.start,
                        usage: stat.rss as f64,
                    });
                }
                current.insert(stat.pid, (stat.start, stat.ticks));
            }
        }
        // Exited/inaccessible processes do not accumulate in memory.
        self.previous = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        format!("42 ({name}) S 1 2 3 4 5 6 7 8 9 10 120 30 0 0 0 0 2 0 900 100000 20 0 0")
    }

    #[test]
    fn parses_unusual_names_ticks_and_rss_without_reading_command_arguments() {
        let stat = parse(&fixture("worker ) (\x1b\n"), 4096).unwrap();
        assert_eq!(stat.pid, 42);
        assert_eq!(stat.name, "worker ) (??");
        assert_eq!(stat.start, 900);
        assert_eq!(stat.ticks, 150);
        assert_eq!(stat.rss, 81920);
        assert!(parse("42 (broken) S 1", 4096).is_none());
        assert!(parse(&fixture("x"), u64::MAX).is_none());
    }

    #[test]
    fn cpu_is_machine_share_and_rejects_pid_reuse_resets_and_missing_baselines() {
        let stat = parse(&fixture("worker"), 4096).unwrap();
        assert_eq!(cpu_usage(&stat, Some(&(900, 100)), Some(400)), Some(12.5));
        for previous in [None, Some(&(901, 100)), Some(&(900, 151))] {
            assert_eq!(cpu_usage(&stat, previous, Some(400)), None);
        }
        assert_eq!(cpu_usage(&stat, Some(&(900, 100)), Some(0)), None);
        assert_eq!(cpu_usage(&stat, Some(&(900, 100)), Some(1)), None);
    }
}
