use std::collections::{BTreeMap, BTreeSet};
use std::{fs, io, path::Path, time::Duration};

#[derive(Debug, PartialEq, Eq)]
struct DeviceCounters {
    major: u32,
    minor: u32,
    read_sectors: u64,
    write_sectors: u64,
}

#[derive(Debug)]
pub struct DiskCounters {
    devices: BTreeMap<String, DeviceCounters>,
}

#[derive(Debug, PartialEq)]
pub struct DiskRates {
    pub read_bytes_per_second: f64,
    pub write_bytes_per_second: f64,
}

impl DiskCounters {
    fn parse(diskstats: &str, selected: &BTreeSet<String>) -> io::Result<Self> {
        let mut devices = BTreeMap::new();
        for line in diskstats.lines().filter(|line| !line.trim().is_empty()) {
            let mut fields = line.split_whitespace();
            let major = fields.next();
            let minor = fields.next();
            let name = fields
                .next()
                .ok_or_else(|| invalid_data("missing disk name"))?;
            if !selected.contains(name) {
                continue;
            }
            let major = major
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid_data("invalid disk major number"))?;
            let minor = minor
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| invalid_data("invalid disk minor number"))?;
            // Read the original 11 counters; newer discard/flush fields are irrelevant.
            let mut counters = [0_u64; 11];
            for counter in &mut counters {
                *counter = fields
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| invalid_data("missing or invalid disk counter"))?;
            }
            let counters = DeviceCounters {
                major,
                minor,
                read_sectors: counters[2],
                write_sectors: counters[6],
            };
            if devices.insert(name.to_owned(), counters).is_some() {
                return Err(invalid_data("duplicate disk"));
            }
        }
        if devices.len() != selected.len() {
            return Err(invalid_data("selected disk missing from /proc/diskstats"));
        }
        Ok(Self { devices })
    }

    pub fn rates_since(&self, previous: &Self, elapsed: Duration) -> Option<DiskRates> {
        if elapsed.is_zero()
            || self.devices.is_empty()
            || !self.devices.keys().eq(previous.devices.keys())
        {
            return None;
        }
        let mut read_delta = 0_u64;
        let mut write_delta = 0_u64;
        for (name, current) in &self.devices {
            let before = previous.devices.get(name)?;
            if (current.major, current.minor) != (before.major, before.minor) {
                return None;
            }
            // Subtract before converting to floating point, and reject resets
            // per disk so activity on another disk cannot mask them.
            read_delta =
                read_delta.checked_add(current.read_sectors.checked_sub(before.read_sectors)?)?;
            write_delta = write_delta
                .checked_add(current.write_sectors.checked_sub(before.write_sectors)?)?;
        }
        // Kernel I/O statistics always use 512-byte sectors, even on 4K disks.
        // https://docs.kernel.org/block/stat.html
        let bytes_per_sector = 512.0;
        let seconds = elapsed.as_secs_f64();
        Some(DiskRates {
            read_bytes_per_second: read_delta as f64 * bytes_per_sector / seconds,
            write_bytes_per_second: write_delta as f64 * bytes_per_sector / seconds,
        })
    }
}

pub fn read_counters() -> io::Result<DiskCounters> {
    let selected = select_devices(Path::new("/sys/block"))?;
    DiskCounters::parse(&fs::read_to_string("/proc/diskstats")?, &selected)
}

fn select_devices(sys_block: &Path) -> io::Result<BTreeSet<String>> {
    let mut selected = BTreeSet::new();
    // /sys/block lists whole devices, excluding their partitions. Only inspect
    // kernel metadata here; never walk mounted filesystems or open disk devices.
    for entry in fs::read_dir(sys_block)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| invalid_data("invalid disk name"))?;
        if ["loop", "ram", "zram", "nullb"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }
        // Count backing disks instead of also counting stacked LVM, dm-crypt
        // or software RAID devices. Keep VM disks such as vda and xvda.
        if fs::read_dir(entry.path().join("slaves"))?
            .next()
            .transpose()?
            .is_some()
        {
            continue;
        }
        selected.insert(name);
    }
    Ok(selected)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn selected(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn fixture(disks: &[(&str, u64, u64)]) -> DiskCounters {
        let mut text = String::new();
        let mut names = BTreeSet::new();
        for (name, read, write) in disks {
            text.push_str(&format!(
                "8 0 {name} 10 2 {read} 4 20 6 {write} 8 0 10 11\n"
            ));
            names.insert((*name).to_owned());
        }
        DiskCounters::parse(&text, &names).unwrap()
    }

    #[test]
    fn parses_sectors_and_ignores_unselected_partitions_and_extra_fields() {
        let text = "259 0 nvme0n1 10 2 100 4 20 6 200 8 0 10 11 12 13 14 15 16 17\n\
                    259 1 nvme0n1p1 10 2 100 4 20 6 200 8 0 10 11\n\
                    7 0 loop0 10 2 900 4 20 6 900 8 0 10 11\n";
        let counters = DiskCounters::parse(text, &selected(&["nvme0n1"])).unwrap();
        assert_eq!(counters.devices.len(), 1);
        assert_eq!(
            counters.devices["nvme0n1"],
            DeviceCounters {
                major: 259,
                minor: 0,
                read_sectors: 100,
                write_sectors: 200,
            }
        );
    }

    #[test]
    fn calculates_combined_rates_with_512_byte_sectors_and_actual_time() {
        let before = fixture(&[("sda", 100, 200), ("vda", 300, 400)]);
        let after = fixture(&[("vda", 304, 406), ("sda", 102, 203)]);
        assert_eq!(
            after.rates_since(&before, Duration::from_millis(500)),
            Some(DiskRates {
                read_bytes_per_second: 6144.0,
                write_bytes_per_second: 9216.0,
            })
        );
    }

    #[test]
    fn idle_disks_have_zero_rates_but_absent_disks_are_unavailable() {
        let idle = fixture(&[("sda", 100, 200)]);
        assert_eq!(
            idle.rates_since(&idle, Duration::from_secs(1)),
            Some(DiskRates {
                read_bytes_per_second: 0.0,
                write_bytes_per_second: 0.0,
            })
        );
        let empty = fixture(&[]);
        assert_eq!(empty.rates_since(&empty, Duration::from_secs(1)), None);
        assert_eq!(idle.rates_since(&idle, Duration::ZERO), None);
    }

    #[test]
    fn rejects_resets_in_either_direction_and_recovers_with_new_baseline() {
        let before = fixture(&[("sda", 100, 200), ("vda", 100, 200)]);
        for (read, write) in [(99, 210), (110, 199), (0, 0)] {
            let reset = fixture(&[("sda", read, write), ("vda", 10000, 20000)]);
            assert_eq!(reset.rates_since(&before, Duration::from_secs(5)), None);
            let next = fixture(&[("sda", read + 5, write + 10), ("vda", 10000, 20000)]);
            assert_eq!(
                next.rates_since(&reset, Duration::from_secs(5)),
                Some(DiskRates {
                    read_bytes_per_second: 512.0,
                    write_bytes_per_second: 1024.0,
                })
            );
        }
    }

    #[test]
    fn device_addition_removal_or_replacement_invalidates_interval() {
        let before = fixture(&[("sda", 100, 200)]);
        for after in [
            fixture(&[]),
            fixture(&[("sda", 200, 300), ("sdb", 9000000, 9000000)]),
            fixture(&[("sdb", 200, 300)]),
            DiskCounters::parse("8 16 sda 1 2 200 4 5 6 300 8 0 10 11", &selected(&["sda"]))
                .unwrap(),
        ] {
            assert_eq!(after.rates_since(&before, Duration::from_secs(1)), None);
        }
    }

    #[test]
    fn preserves_small_deltas_on_large_counters_and_rejects_overflow() {
        let before = fixture(&[("sda", u64::MAX - 1, u64::MAX - 2)]);
        let after = fixture(&[("sda", u64::MAX, u64::MAX)]);
        assert_eq!(
            after.rates_since(&before, Duration::from_secs(1)),
            Some(DiskRates {
                read_bytes_per_second: 512.0,
                write_bytes_per_second: 1024.0,
            })
        );
        let wrapped = fixture(&[("sda", 0, 0)]);
        assert_eq!(wrapped.rates_since(&before, Duration::from_secs(1)), None);
        let before = fixture(&[("sda", 0, 0), ("sdb", 0, 0)]);
        let after = fixture(&[("sda", u64::MAX, u64::MAX), ("sdb", 1, 1)]);
        assert_eq!(after.rates_since(&before, Duration::from_secs(1)), None);
    }

    #[test]
    fn rejects_missing_duplicate_or_invalid_selected_disk_data() {
        let names = selected(&["sda"]);
        let row = "8 0 sda 1 2 3 4 5 6 7 8 0 10 11\n";
        for text in [
            "".to_owned(),
            "8 0".to_owned(),
            "8 0 sda 1 2 3".to_owned(),
            row.replace("8 0 sda", "x 0 sda"),
            row.replace("8 0 sda", "8 -1 sda"),
            row.replace(" 3 ", " invalid "),
            row.replace(" 7 ", " -1 "),
            row.replace(" 3 ", " 18446744073709551616 "),
            format!("{row}{row}"),
        ] {
            assert!(DiskCounters::parse(&text, &names).is_err(), "{text}");
        }
    }

    struct SysfsFixture(PathBuf);

    impl SysfsFixture {
        fn new() -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lantern-disks-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn add(&self, name: &str) {
            fs::create_dir_all(self.0.join(name).join("slaves")).unwrap();
        }
    }

    impl Drop for SysfsFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn selects_whole_backing_disks_and_keeps_vm_disks() {
        let sysfs = SysfsFixture::new();
        for name in [
            "sda", "nvme0n1", "vda", "xvda", "mmcblk0", "loop0", "ram0", "zram0", "nullb0", "dm-0",
            "md0",
        ] {
            sysfs.add(name);
        }
        fs::create_dir(sysfs.0.join("sda/sda1")).unwrap();
        fs::create_dir(sysfs.0.join("dm-0/slaves/sda1")).unwrap();
        fs::create_dir(sysfs.0.join("md0/slaves/nvme0n1")).unwrap();
        assert_eq!(
            select_devices(&sysfs.0).unwrap(),
            selected(&["sda", "nvme0n1", "vda", "xvda", "mmcblk0"])
        );
    }

    #[test]
    fn missing_sysfs_metadata_is_an_error_not_a_fake_zero() {
        let sysfs = SysfsFixture::new();
        assert!(select_devices(&sysfs.0.join("missing")).is_err());
        fs::create_dir(sysfs.0.join("sda")).unwrap();
        assert!(select_devices(&sysfs.0).is_err());
    }
}
