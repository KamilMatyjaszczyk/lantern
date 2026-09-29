use std::{
    ffi::OsString,
    io::{self, Write},
};

pub const HELP: &str = "\
Lantern — lightweight Linux system monitoring

Usage: lantern [OPTION]

Options:
  --graph, -graph  Show graphs and events (default)
  --snapshot      Print one current snapshot
  --status        Show collector detection and saved-history freshness
  --collect       Collect metrics every 5 seconds until stopped
  -h, --help      Print help
  -V, --version   Print version

Without options, open graphs with events analysed from saved samples.

This build shows CPU usage, load averages, CPU temperature, memory, swap,
disk read/write rates, and network receive/transmit rates (excluding loopback).
Optional NVIDIA GPU usage, VRAM, and temperature are shown when available.
The collector stays in the foreground; stop it with Ctrl+C or SIGTERM.
Samples are saved in batches about every 30 seconds and when stopping.
History is stored in $XDG_STATE_HOME/lantern/lantern.db, or by default
~/.local/state/lantern/lantern.db. Set LANTERN_DB to override the database path.
History is retained for about seven days. Each graph page covers up to 24 hours.
In a terminal, graphs use a fixed screen and follow newly saved measurements.
Left/Right = event + matching graph, Up/Down = metric, L/R = time period.
Views: CPU use/temp, RAM/swap, separate RX, TX, disk read and write, optional GPU use/temp and VRAM.
CPU uses left % and right °C axes. Diamond = event peak.
Q = quit.
Redirected graph output prints the latest page once and exits.
";

pub fn write_snapshot(
    output: &mut impl Write,
    snapshot: &crate::model::SystemSample,
    machine: &crate::model::MachineInfo,
) -> io::Result<()> {
    match &machine.hostname {
        Some(hostname) => writeln!(output, "Lantern · {hostname}"),
        None => writeln!(output, "Lantern"),
    }?;
    writeln!(output, "──────────────────────────────────────\n")?;
    let percent = snapshot
        .cpu_percent
        .map_or_else(|| "N/A".into(), |value| format!("{value:.1} %"));
    let temperature = snapshot
        .cpu_temperature
        .map_or_else(|| "N/A".into(), |value| format!("{value:.1} °C"));
    writeln!(output, "CPU       {percent:<12} {temperature}")?;
    writeln!(
        output,
        "Load      {}  {}  {}",
        format_load(snapshot.load_1),
        format_load(snapshot.load_5),
        format_load(snapshot.load_15)
    )?;
    writeln!(output)?;
    print_memory_usage(
        output,
        "Memory",
        snapshot.memory_used,
        snapshot.memory_total,
        snapshot.memory_percent(),
    )?;
    print_memory_usage(
        output,
        "Swap",
        snapshot.swap_used,
        snapshot.swap_total,
        snapshot.swap_percent(),
    )?;
    writeln!(output, "\nDisk")?;
    writeln!(
        output,
        "Read      {}",
        format_byte_rate(snapshot.disk_read_rate)
    )?;
    writeln!(
        output,
        "Write     {}",
        format_byte_rate(snapshot.disk_write_rate)
    )?;
    writeln!(output, "\nNetwork")?;
    writeln!(
        output,
        "Down      {}",
        format_byte_rate(snapshot.network_rx_rate)
    )?;
    writeln!(
        output,
        "Up        {}",
        format_byte_rate(snapshot.network_tx_rate)
    )?;
    if snapshot.has_gpu_data() {
        writeln!(output)?;
        let utilisation = snapshot
            .gpu_utilisation
            .map_or_else(|| "N/A".into(), |value| format!("{value} %"));
        writeln!(output, "GPU       {utilisation}")?;
        match snapshot.gpu_memory_used.zip(snapshot.gpu_memory_total) {
            Some((used, total)) => {
                const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
                writeln!(
                    output,
                    "VRAM      {:.2} / {:.2} GiB",
                    used as f64 / GIB,
                    total as f64 / GIB
                )?;
            }
            None => writeln!(output, "VRAM      N/A")?,
        }
        let temperature = snapshot
            .gpu_temperature
            .map_or_else(|| "N/A".into(), |value| format!("{value} °C"));
        writeln!(output, "Temp      {temperature}")?;
    }
    writeln!(
        output,
        "\nUptime    {}",
        format_uptime(machine.uptime_seconds)
    )
}

fn format_uptime(seconds: Option<u64>) -> String {
    let Some(seconds) = seconds else {
        return "N/A".into();
    };
    let (days, hours, minutes) = (seconds / 86400, seconds % 86400 / 3600, seconds % 3600 / 60);
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m {}s", seconds % 60)
    }
}

fn format_load(value: Option<f64>) -> String {
    value.map_or_else(|| "N/A".into(), |value| format!("{value:.2}"))
}

pub(crate) fn format_byte_rate(rate: Option<f64>) -> String {
    let Some(mut value) = rate.filter(|value| value.is_finite() && *value >= 0.0) else {
        return "N/A".into();
    };
    // Decimal units: 1 kB/s = 1000 bytes/second, not bits/second.
    let units = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s", "PB/s", "EB/s"];
    let mut index = 0;
    while value >= 1000.0 && index < units.len() - 1 {
        value /= 1000.0;
        index += 1;
    }
    if value >= 10000.0 {
        format!("{value:.1e} {}", units[index])
    } else {
        format!("{value:.1} {}", units[index])
    }
}

fn print_memory_usage(
    output: &mut impl Write,
    label: &str,
    used: Option<u64>,
    total: Option<u64>,
    percent: Option<f64>,
) -> io::Result<()> {
    let Some((used, total)) = used.zip(total) else {
        return writeln!(output, "{label:<10}N/A");
    };
    if label == "Swap" && total == 0 && used == 0 {
        return writeln!(output, "{label:<10}disabled (0 GiB)");
    }
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let percent = percent.map_or_else(|| "N/A".into(), |value| format!("{value:.1} %"));
    writeln!(
        output,
        "{label:<10}{:.2} / {:.2} GiB     {percent}",
        used as f64 / GIB,
        total as f64 / GIB,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Snapshot,
    Status,
    Graph,
    Collect,
    Help,
    Version,
}

pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter();
    let Some(argument) = args.next() else {
        return Ok(Command::Graph);
    };

    let command = match argument.to_str() {
        Some("--graph" | "-graph") => Command::Graph,
        Some("--snapshot") => Command::Snapshot,
        Some("--status") => Command::Status,
        Some("--collect") => Command::Collect,
        Some("--help" | "-h") => Command::Help,
        Some("--version" | "-V") => Command::Version,
        _ => return Err(format!("unknown argument: {argument:?}")),
    };

    if let Some(extra) = args.next() {
        return Err(format!(
            "unexpected argument: {extra:?}; use only one option at a time"
        ));
    }

    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_snapshot_metrics_are_not_fabricated() {
        let mut output = Vec::new();
        let mut sample = crate::model::SystemSample::new(0);
        let machine = crate::model::MachineInfo::default();
        write_snapshot(&mut output, &sample, &machine).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Memory    N/A"));
        assert!(text.contains("Uptime    N/A"));
        assert!(!text.contains("GPU"));
        sample.gpu_temperature = Some(44);
        sample.swap_used = Some(0);
        sample.swap_total = Some(0);
        let mut output = Vec::new();
        write_snapshot(&mut output, &sample, &machine).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("GPU       N/A\nVRAM      N/A\nTemp      44 °C"));
        assert!(text.contains("Swap      disabled (0 GiB)"));
    }

    #[test]
    fn byte_rates_keep_units_and_invalid_values_explicit() {
        assert_eq!(format_byte_rate(Some(0.0)), "0.0 B/s");
        assert_eq!(format_byte_rate(Some(1250.0)), "1.2 kB/s");
        assert_eq!(format_byte_rate(Some(1_500_000.0)), "1.5 MB/s");
        for value in [None, Some(-1.0), Some(f64::NAN), Some(f64::INFINITY)] {
            assert_eq!(format_byte_rate(value), "N/A");
        }
        assert!(format_byte_rate(Some(f64::MAX)).len() < 30);
    }

    #[test]
    fn uptime_format_handles_day_and_hour_boundaries() {
        assert_eq!(format_uptime(Some(59)), "0m 59s");
        assert_eq!(format_uptime(Some(3600)), "1h 0m");
        assert_eq!(format_uptime(Some(86400)), "1d 0h 0m");
        assert_eq!(
            format_uptime(Some(12 * 86400 + 4 * 3600 + 21 * 60)),
            "12d 4h 21m"
        );
    }

    fn parse_args(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_selects_graph() {
        assert_eq!(parse_args(&[]), Ok(Command::Graph));
    }

    #[test]
    fn accepts_supported_options_and_aliases() {
        for (option, expected) in [
            ("--graph", Command::Graph),
            ("-graph", Command::Graph),
            ("--collect", Command::Collect),
            ("--snapshot", Command::Snapshot),
            ("--status", Command::Status),
            ("--help", Command::Help),
            ("-h", Command::Help),
            ("--version", Command::Version),
            ("-V", Command::Version),
        ] {
            assert_eq!(parse_args(&[option]), Ok(expected));
        }
    }

    #[test]
    fn rejects_unknown_arguments() {
        for argument in ["--unknown", "graph", "--graph=true", ""] {
            assert!(parse_args(&[argument]).is_err());
        }
    }

    #[test]
    fn rejects_conflicting_modes_and_extra_arguments() {
        for args in [
            ["--graph", "--collect"],
            ["--collect", "--graph"],
            ["--graph", "--graph"],
            ["--help", "unexpected"],
        ] {
            assert!(parse_args(&args).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_non_utf8_arguments_without_panicking() {
        use std::os::unix::ffi::OsStringExt;

        assert!(parse([OsString::from_vec(vec![0xff])]).is_err());
    }
}
