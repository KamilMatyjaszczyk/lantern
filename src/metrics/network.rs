use std::collections::BTreeMap;
use std::{fs, io, time::Duration};

#[derive(Debug, PartialEq, Eq)]
struct InterfaceCounters {
    rx_bytes: u64,
    tx_bytes: u64,
    health: [u64; 4],
    group: usize,
}

#[derive(Debug)]
pub struct NetworkCounters {
    interfaces: BTreeMap<String, InterfaceCounters>,
}

#[derive(Debug, PartialEq)]
pub struct NetworkRates {
    pub rx_bytes_per_second: f64,
    pub tx_bytes_per_second: f64,
}

impl NetworkCounters {
    pub fn parse(net_dev: &str) -> io::Result<Self> {
        let mut lines = net_dev.lines();
        let valid_header = lines
            .next()
            .is_some_and(|line| line.contains("Receive") && line.contains("Transmit"))
            && lines.next().is_some_and(|line| line.contains("bytes"));
        if !valid_header {
            return Err(invalid_data("missing network counter headers"));
        }

        let mut interfaces = BTreeMap::new();
        for line in lines.filter(|line| !line.trim().is_empty()) {
            let (name, values) = line
                .rsplit_once(':')
                .ok_or_else(|| invalid_data("missing network interface separator"))?;
            let name = name.trim();
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                return Err(invalid_data("invalid network interface name"));
            }
            if name == "lo" {
                continue;
            }
            let mut fields = values.split_whitespace();
            let mut counters = [0_u64; 16];
            for counter in &mut counters {
                *counter = fields
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| invalid_data("missing or invalid network counter"))?;
            }
            let counters = InterfaceCounters {
                rx_bytes: counters[0],
                tx_bytes: counters[8],
                health: [counters[2], counters[10], counters[3], counters[11]],
                group: 3,
            };
            if interfaces.insert(name.to_owned(), counters).is_some() {
                return Err(invalid_data("duplicate network interface"));
            }
        }
        Ok(Self { interfaces })
    }

    pub fn grouped_rates_since(
        &self,
        previous: &Self,
        elapsed: Duration,
    ) -> crate::model::NetworkGroups {
        let mut result = [[None; 6]; 4];
        if elapsed.is_zero() {
            return result;
        }
        for (group, rates) in result.iter_mut().enumerate() {
            let members = |c: &InterfaceCounters| c.group == group;
            let current = self.interfaces.iter().filter(|(_, c)| members(c));
            if current.clone().next().is_none()
                || !current.clone().map(|(name, _)| name).eq(previous
                    .interfaces
                    .iter()
                    .filter(|(_, c)| members(c))
                    .map(|(name, _)| name))
            {
                continue;
            }
            let mut deltas = [Some(0_u64); 6];
            for (name, now) in current {
                let before = &previous.interfaces[name];
                let values = |c: &InterfaceCounters| {
                    [
                        c.rx_bytes,
                        c.tx_bytes,
                        c.health[0],
                        c.health[1],
                        c.health[2],
                        c.health[3],
                    ]
                };
                for ((sum, now), before) in deltas.iter_mut().zip(values(now)).zip(values(before)) {
                    *sum = sum.and_then(|sum| sum.checked_add(now.checked_sub(before)?));
                }
            }
            *rates = deltas.map(|delta| delta.map(|v| v as f64 / elapsed.as_secs_f64()));
        }
        result
    }

    pub fn rates_since(&self, previous: &Self, elapsed: Duration) -> Option<NetworkRates> {
        // A topology change needs a new baseline. Do not subtract totals across
        // different sets of interfaces or report a misleading partial rate.
        if elapsed.is_zero() || !self.interfaces.keys().eq(previous.interfaces.keys()) {
            return None;
        }
        let mut rx_delta = 0_u64;
        let mut tx_delta = 0_u64;
        for (name, current) in &self.interfaces {
            let before = previous.interfaces.get(name)?;
            // Check each interface separately so traffic on another interface
            // cannot mask a counter reset or wraparound.
            rx_delta = rx_delta.checked_add(current.rx_bytes.checked_sub(before.rx_bytes)?)?;
            tx_delta = tx_delta.checked_add(current.tx_bytes.checked_sub(before.tx_bytes)?)?;
        }
        let seconds = elapsed.as_secs_f64();
        Some(NetworkRates {
            rx_bytes_per_second: rx_delta as f64 / seconds,
            tx_bytes_per_second: tx_delta as f64 / seconds,
        })
    }
}

pub fn read_counters() -> io::Result<NetworkCounters> {
    let mut counters = NetworkCounters::parse(&fs::read_to_string("/proc/net/dev")?)?;
    for (name, counters) in &mut counters.interfaces {
        counters.group = classify(&std::path::Path::new("/sys/class/net").join(name));
    }
    Ok(counters)
}

fn classify(path: &std::path::Path) -> usize {
    let Ok(real) = fs::canonicalize(path) else {
        return 3;
    };
    if real.starts_with("/sys/devices/virtual") {
        return 2;
    }
    if path.join("wireless").is_dir() || path.join("phy80211").exists() {
        return 1;
    }
    // Only hardware-backed Ethernet devices (ARPHRD_ETHER); do not guess by name.
    if path.join("device").exists()
        && fs::read_to_string(path.join("type")).is_ok_and(|s| s.trim() == "1")
    {
        return 0;
    }
    3
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
";

    fn fixture(interfaces: &[(&str, u64, u64)]) -> NetworkCounters {
        let mut text = HEADER.to_owned();
        for (name, rx, tx) in interfaces {
            text.push_str(&format!(
                "  {name}: {rx} 1 2 3 4 5 6 7 {tx} 8 9 10 11 12 13 14\n"
            ));
        }
        NetworkCounters::parse(&text).unwrap()
    }

    #[test]
    fn groups_isolate_virtual_topology_changes_and_reset_each_counter_independently() {
        let mut before = fixture(&[("eth0", 100, 200), ("veth0", 100, 200)]);
        before.interfaces.get_mut("eth0").unwrap().group = 0;
        before.interfaces.get_mut("veth0").unwrap().group = 2;
        let mut after = fixture(&[("eth0", 600, 1200), ("veth1", 999999, 999999)]);
        let eth = after.interfaces.get_mut("eth0").unwrap();
        eth.group = 0;
        eth.health = [12, 14, 18, 30];
        after.interfaces.get_mut("veth1").unwrap().group = 2;
        let rates = after.grouped_rates_since(&before, Duration::from_secs(5));
        assert_eq!(
            rates[0],
            [
                Some(100.0),
                Some(200.0),
                Some(2.0),
                Some(1.0),
                Some(3.0),
                Some(4.0)
            ]
        );
        assert_eq!(rates[2], [None; 6]);
        assert_eq!(rates[1], [None; 6]);
        after.interfaces.get_mut("eth0").unwrap().health[0] = 0;
        let rates = after.grouped_rates_since(&before, Duration::from_secs(5));
        assert_eq!(rates[0][0], Some(100.0));
        assert_eq!(rates[0][2], None);
        assert_eq!(rates[0][3], Some(1.0));
        assert_eq!(
            after.grouped_rates_since(&before, Duration::ZERO),
            [[None; 6]; 4]
        );
        before.interfaces.get_mut("eth0").unwrap().group = 3;
        assert_eq!(
            after.grouped_rates_since(&before, Duration::from_secs(5))[0],
            [None; 6]
        );
    }

    #[test]
    fn idle_group_has_measured_zero_but_absent_groups_do_not() {
        let c = fixture(&[("eth0", 100, 200)]);
        let rates = c.grouped_rates_since(&c, Duration::from_secs(5));
        assert_eq!(rates[3], [Some(0.0); 6]);
        assert_eq!(rates[0], [None; 6]);
        assert_eq!(rates[1], [None; 6]);
        assert_eq!(rates[2], [None; 6]);
    }

    #[test]
    fn classification_uses_device_metadata_and_keeps_missing_devices_unknown() {
        let root = std::env::temp_dir().join(format!("lantern-net-class-{}", std::process::id()));
        fs::create_dir_all(root.join("device")).unwrap();
        fs::write(root.join("type"), "1\n").unwrap();
        assert_eq!(classify(&root), 0);
        fs::create_dir(root.join("wireless")).unwrap();
        assert_eq!(classify(&root), 1);
        fs::remove_dir_all(&root).unwrap();
        assert_eq!(classify(&root), 3);
        // Loopback is skipped during parsing but belongs to sysfs' virtual tree.
        if std::path::Path::new("/sys/class/net/lo").exists() {
            assert_eq!(classify(std::path::Path::new("/sys/class/net/lo")), 2);
        }
    }

    #[test]
    fn parses_byte_counters_and_ignores_loopback() {
        let counters = fixture(&[
            ("lo", 999999, 999999),
            ("enp3s0", 1234, 5678),
            ("wlp2s0", 9000, 10000),
        ]);
        assert_eq!(counters.interfaces.len(), 2);
        assert_eq!(
            counters.interfaces["enp3s0"],
            InterfaceCounters {
                rx_bytes: 1234,
                tx_bytes: 5678,
                health: [2, 9, 3, 10],
                group: 3,
            }
        );
        assert_eq!(counters.interfaces["wlp2s0"].rx_bytes, 9000);
    }

    #[test]
    fn calculates_combined_rates_using_actual_elapsed_time() {
        let before = fixture(&[("eth0", 1000, 2000), ("wlan0", 3000, 4000)]);
        // Reverse the input order to verify matching by name.
        let after = fixture(&[("wlan0", 6000, 5000), ("eth0", 3000, 2500)]);
        assert_eq!(
            after.rates_since(&before, Duration::from_millis(250)),
            Some(NetworkRates {
                rx_bytes_per_second: 20000.0,
                tx_bytes_per_second: 6000.0,
            })
        );
    }

    #[test]
    fn idle_interfaces_and_loopback_only_have_zero_rates() {
        for counters in [
            fixture(&[("eth0", 1000, 2000)]),
            fixture(&[("lo", 999, 999)]),
            fixture(&[]),
        ] {
            assert_eq!(
                counters.rates_since(&counters, Duration::from_secs(1)),
                Some(NetworkRates {
                    rx_bytes_per_second: 0.0,
                    tx_bytes_per_second: 0.0,
                })
            );
        }
    }

    #[test]
    fn rejects_zero_elapsed_time() {
        let counters = fixture(&[("eth0", 1000, 2000)]);
        assert_eq!(counters.rates_since(&counters, Duration::ZERO), None);
    }

    #[test]
    fn rejects_reset_in_either_direction_even_if_other_interfaces_grow() {
        let before = fixture(&[("eth0", 1000, 2000), ("wlan0", 3000, 4000)]);
        for (rx, tx) in [(999, 2100), (1100, 1999), (0, 0)] {
            let after = fixture(&[("eth0", rx, tx), ("wlan0", 10000, 10000)]);
            assert_eq!(after.rates_since(&before, Duration::from_secs(5)), None);
        }
    }

    #[test]
    fn topology_changes_require_a_new_baseline() {
        let before = fixture(&[("eth0", 1000, 2000)]);
        for interfaces in [
            vec![("eth0", 2000, 3000), ("wlan0", 9000000000, 8000000000)],
            vec![],
            vec![("wlan0", 2000, 3000)],
        ] {
            let after = fixture(&interfaces);
            assert_eq!(after.rates_since(&before, Duration::from_secs(5)), None);
        }
    }

    #[test]
    fn resumes_after_counter_reset_with_next_baseline() {
        let before = fixture(&[("eth0", 1000, 2000)]);
        let reset = fixture(&[("eth0", 0, 0)]);
        let next = fixture(&[("eth0", 500, 250)]);
        assert_eq!(reset.rates_since(&before, Duration::from_secs(5)), None);
        assert_eq!(
            next.rates_since(&reset, Duration::from_secs(5)),
            Some(NetworkRates {
                rx_bytes_per_second: 100.0,
                tx_bytes_per_second: 50.0,
            })
        );
    }

    #[test]
    fn subtracts_large_integer_counters_before_float_conversion() {
        let before = fixture(&[("eth0", u64::MAX - 100, u64::MAX - 200)]);
        let after = fixture(&[("eth0", u64::MAX, u64::MAX)]);
        assert_eq!(
            after.rates_since(&before, Duration::from_secs(1)),
            Some(NetworkRates {
                rx_bytes_per_second: 100.0,
                tx_bytes_per_second: 200.0,
            })
        );
        let wrapped = fixture(&[("eth0", 10, 10)]);
        assert_eq!(wrapped.rates_since(&before, Duration::from_secs(1)), None);
    }

    #[test]
    fn rejects_overflow_when_summing_deltas() {
        let before = fixture(&[("eth0", 0, 0), ("wlan0", 0, 0)]);
        let after = fixture(&[("eth0", u64::MAX, u64::MAX), ("wlan0", 1, 1)]);
        assert_eq!(after.rates_since(&before, Duration::from_secs(1)), None);
    }

    #[test]
    fn rejects_missing_headers_and_malformed_rows() {
        assert!(NetworkCounters::parse("").is_err());
        assert!(NetworkCounters::parse("not network statistics\n").is_err());
        for row in [
            "eth0 100 200",
            "eth0: 100 200",
            ": 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16",
            "eth 0: 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16",
            "eth0: invalid 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0",
            "eth0: -1 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0",
            "eth0: 18446744073709551616 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0",
            "eth0: 100 0 0 0 0 0 0 0 invalid 0 0 0 0 0 0 0",
        ] {
            assert!(
                NetworkCounters::parse(&format!("{HEADER}{row}\n")).is_err(),
                "{row}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_interfaces() {
        let row = "eth0: 100 0 0 0 0 0 0 0 100 0 0 0 0 0 0 0\n";
        assert!(NetworkCounters::parse(&format!("{HEADER}{row}{row}")).is_err());
    }
}
