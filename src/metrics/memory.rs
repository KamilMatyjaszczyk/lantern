use std::{fs, io};

#[derive(Debug, PartialEq, Eq)]
pub struct Usage {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl Usage {
    fn new(total_bytes: u64, available_bytes: u64) -> Option<Self> {
        (available_bytes <= total_bytes).then_some(Self {
            total_bytes,
            available_bytes,
        })
    }

    pub fn used_bytes(&self) -> u64 {
        self.total_bytes - self.available_bytes
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct MemoryInfo {
    pub memory: Option<Usage>,
    pub swap: Option<Usage>,
}

impl MemoryInfo {
    pub fn parse(meminfo: &str) -> Self {
        let mut memory_total = None;
        let mut memory_available = None;
        let mut swap_total = None;
        let mut swap_free = None;

        for line in meminfo.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let field = match key {
                "MemTotal" => &mut memory_total,
                "MemAvailable" => &mut memory_available,
                "SwapTotal" => &mut swap_total,
                "SwapFree" => &mut swap_free,
                _ => continue,
            };
            *field = parse_bytes(value);
        }

        Self {
            // Do not substitute MemFree: that would count reclaimable cache as used.
            // On older kernels without MemAvailable, report memory usage as N/A.
            memory: memory_total
                .filter(|total| *total > 0)
                .zip(memory_available)
                .and_then(|(total, available)| Usage::new(total, available)),
            swap: swap_total
                .zip(swap_free)
                .and_then(|(total, free)| Usage::new(total, free)),
        }
    }
}

pub fn read_memory() -> io::Result<MemoryInfo> {
    Ok(MemoryInfo::parse(&fs::read_to_string("/proc/meminfo")?))
}

fn parse_bytes(value: &str) -> Option<u64> {
    let mut fields = value.split_whitespace();
    let kibibytes = fields.next()?.parse::<u64>().ok()?;
    if fields.next()? != "kB" || fields.next().is_some() {
        return None;
    }
    // Despite the kernel's label, /proc/meminfo uses units of 1024 bytes.
    kibibytes.checked_mul(1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "\
MemTotal:        8192 kB
MemFree:          512 kB
MemAvailable:    3072 kB
Buffers:          256 kB
Cached:          2304 kB
SwapTotal:       2048 kB
SwapFree:         512 kB
HugePages_Total:    0
";

    #[test]
    fn uses_available_memory_and_converts_to_bytes() {
        let info = MemoryInfo::parse(MEMINFO);
        let memory = info.memory.unwrap();
        assert_eq!(memory.total_bytes, 8192 * 1024);
        assert_eq!(memory.available_bytes, 3072 * 1024);
        assert_eq!(memory.used_bytes(), 5120 * 1024);
        let swap = info.swap.unwrap();
        assert_eq!(swap.total_bytes, 2048 * 1024);
        assert_eq!(swap.available_bytes, 512 * 1024);
        assert_eq!(swap.used_bytes(), 1536 * 1024);
    }

    #[test]
    fn parses_reordered_fields_and_whitespace() {
        let info = MemoryInfo::parse(
            "SwapFree:\t512 kB\nMemAvailable: 3072 kB\nSwapTotal: 2048 kB\nMemTotal: 8192 kB\n",
        );
        assert_eq!(info, MemoryInfo::parse(MEMINFO));
    }

    #[test]
    fn no_swap_is_distinct_from_missing_swap_data() {
        let info = MemoryInfo::parse("SwapTotal: 0 kB\nSwapFree: 0 kB\n");
        let swap = info.swap.unwrap();
        assert_eq!(swap.total_bytes, 0);
        assert_eq!(swap.used_bytes(), 0);
        assert_eq!(MemoryInfo::parse("SwapTotal: 0 kB\n").swap, None);
        assert_eq!(MemoryInfo::parse("").swap, None);
    }

    #[test]
    fn missing_available_memory_does_not_treat_cache_as_used() {
        let info = MemoryInfo::parse(
            "MemTotal: 8192 kB\nMemFree: 512 kB\nSwapTotal: 2048 kB\nSwapFree: 512 kB\n",
        );
        assert_eq!(info.memory, None);
        assert_eq!(info.swap.unwrap().used_bytes(), 1536 * 1024);
    }

    #[test]
    fn invalid_memory_does_not_hide_valid_swap() {
        for value in [
            "garbage kB",
            "-1 kB",
            "8193 kB",
            "10 MB",
            "10",
            "10 kB extra",
            "18446744073709551615 kB",
            "18446744073709551616 kB",
        ] {
            let fixture = format!(
                "MemTotal: 8192 kB\nMemAvailable: {value}\nSwapTotal: 2048 kB\nSwapFree: 512 kB\n"
            );
            let info = MemoryInfo::parse(&fixture);
            assert_eq!(info.memory, None, "{value}");
            assert!(info.swap.is_some(), "{value}");
        }
    }

    #[test]
    fn invalid_swap_does_not_hide_valid_memory() {
        for value in ["garbage kB", "2049 kB", "-1 kB"] {
            let fixture =
                MEMINFO.replace("SwapFree:         512 kB", &format!("SwapFree: {value}"));
            let info = MemoryInfo::parse(&fixture);
            assert_eq!(info.swap, None);
            assert!(info.memory.is_some());
        }
    }

    #[test]
    fn handles_empty_input_and_zero_memory() {
        assert_eq!(MemoryInfo::parse(""), MemoryInfo::default());
        assert_eq!(
            MemoryInfo::parse("MemTotal: 0 kB\nMemAvailable: 0 kB\n").memory,
            None
        );
    }

    #[test]
    fn handles_fully_available_and_fully_used_memory() {
        for (available, expected_used) in [(8192, 0), (0, 8192 * 1024)] {
            let fixture = format!("MemTotal: 8192 kB\nMemAvailable: {available} kB\n");
            assert_eq!(
                MemoryInfo::parse(&fixture).memory.unwrap().used_bytes(),
                expected_used
            );
        }
    }
}
