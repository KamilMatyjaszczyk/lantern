use std::{fs, io};

use crate::model::MachineInfo;

pub fn read_machine_info() -> MachineInfo {
    MachineInfo {
        hostname: fs::read_to_string("/proc/sys/kernel/hostname")
            .ok()
            .map(|name| {
                name.trim()
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .collect::<String>()
            })
            .filter(|name| !name.is_empty()),
        uptime_seconds: fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| parse_uptime(&text).ok()),
    }
}

fn parse_uptime(text: &str) -> io::Result<u64> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid uptime");
    let value = text.split_whitespace().next().ok_or_else(invalid)?;
    let whole = if let Some((whole, fraction)) = value.split_once('.') {
        if fraction.is_empty() || !fraction.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        whole
    } else {
        value
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    whole.parse().map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_uses_first_field_and_truncates_fractional_seconds() {
        assert_eq!(parse_uptime("123.99 980.10\n").unwrap(), 123);
        assert_eq!(parse_uptime("0.01 0.00").unwrap(), 0);
        assert_eq!(parse_uptime("86400 1").unwrap(), 86400);
        for input in [
            "",
            "NaN",
            "-1.0 0",
            "1.x",
            ".5",
            "1.",
            "18446744073709551616",
        ] {
            assert!(parse_uptime(input).is_err(), "{input}");
        }
    }
}
