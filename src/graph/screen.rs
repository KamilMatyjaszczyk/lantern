//! Bounded, coloured terminal frames. Text and colour are separate so ANSI
//! escapes never participate in clipping or terminal-width calculations.
use super::{
    MARGIN, Pages, columns, curve, glyph, local_time, rate_scale, temperature_top, time_axis,
};
use crossterm::{
    cursor::MoveTo,
    queue,
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{Clear, ClearType},
};
use lantern::{
    events::{Event, Events, Kind},
    history::History,
};
use std::{
    io::{self, Write},
    ops::Deref,
};

const TITLES: [&str; 8] = [
    "CPU",
    "RAM / swap",
    "Network RX",
    "Network TX",
    "Disk read",
    "Disk write",
    "GPU",
    "VRAM",
];
const SERIES: [usize; 8] = [0, 2, 3, 4, 7, 8, 5, 10];
const SECONDARY: [Option<usize>; 8] = [Some(1), Some(6), None, None, None, None, Some(9), None];
const LABELS: [(&str, &str); 8] = [
    ("CPU", "Temp"),
    ("RAM", "Swap"),
    ("RX", ""),
    ("TX", ""),
    ("Read", ""),
    ("Write", ""),
    ("GPU", "Temp"),
    ("VRAM", ""),
];
const COLORS: [Color; 8] = [
    Color::Cyan,
    Color::Green,
    Color::Cyan,
    Color::Yellow,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Green,
];
const SECOND_COLORS: [Color; 8] = [
    TEMP_COLOR,
    Color::Cyan,
    Color::Yellow,
    Color::Yellow,
    Color::Yellow,
    Color::Yellow,
    TEMP_COLOR,
    Color::Green,
];
const TEMP_COLOR: Color = Color::Yellow;
const EVENT_COLOR: Color = Color::White;

pub(super) fn event_metric(kind: Kind) -> usize {
    match kind {
        Kind::Cpu | Kind::Temperature => 0,
        Kind::Memory | Kind::Swap => 1,
        Kind::NetworkRx => 2,
        Kind::NetworkTx => 3,
        Kind::DiskRead => 4,
        Kind::DiskWrite => 5,
        Kind::Gpu => 6,
    }
}

pub(super) fn metric_count(history: &History) -> usize {
    if [5, 9, 10]
        .iter()
        .any(|i| history.series[*i].summary.mean().is_some())
    {
        8
    } else {
        6
    }
}

pub(super) fn plot_width(width: u16) -> usize {
    // Reserve both axes for the CPU overlay and one cell to prevent wrapping.
    columns(usize::from(width).saturating_sub(9))
}

fn value(series: usize, reading: Option<f64>) -> String {
    if matches!(series, 3 | 4 | 7 | 8) {
        crate::cli::format_byte_rate(reading)
    } else {
        reading.map_or_else(
            || "N/A".into(),
            |v| {
                format!(
                    "{v:.1} {}",
                    if matches!(series, 1 | 9) { "°C" } else { "%" }
                )
            },
        )
    }
}

struct Paint {
    row: usize,
    column: usize,
    text: String,
    color: Color,
}

pub(super) struct Frame {
    lines: Vec<String>,
    paints: Vec<Paint>,
}
impl Deref for Frame {
    type Target = [String];
    fn deref(&self) -> &Self::Target {
        &self.lines
    }
}
impl Frame {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            paints: Vec::new(),
        }
    }
    fn push(&mut self, text: impl Into<String>) {
        self.lines.push(text.into());
    }
    fn colored(&mut self, text: impl Into<String>, color: Color) {
        let text = text.into();
        self.paints.push(Paint {
            row: self.lines.len(),
            column: 0,
            text: text.clone(),
            color,
        });
        self.push(text);
    }
    fn segment(&mut self, row: usize, column: usize, text: String, color: Color) {
        self.paints.push(Paint {
            row,
            column,
            text,
            color,
        });
    }
    fn fit(mut self, width: usize, height: usize) -> Self {
        self.lines.truncate(height);
        for line in &mut self.lines {
            *line = line.chars().take(width).collect();
        }
        self.paints
            .retain(|p| p.row < self.lines.len() && p.column < width);
        for p in &mut self.paints {
            let available = self.lines[p.row]
                .chars()
                .count()
                .min(width)
                .saturating_sub(p.column);
            p.text = p.text.chars().take(available).collect();
        }
        self
    }
}

/// Same half-open bucketing as History::add, with widened arithmetic.
fn event_column(history: &History, event: &Event) -> Option<usize> {
    let time = event.peak_time;
    if time < history.start || time >= history.end {
        return None;
    }
    Some(
        ((i128::from(time) - i128::from(history.start)) * history.series[0].buckets.len() as i128
            / (i128::from(history.end) - i128::from(history.start))) as usize,
    )
}

/// Each rate graph uses its own raw peak; CPU intentionally has two units.
fn shared_scale(history: &History, selected: usize) -> (f64, f64, &'static str) {
    let peak = std::iter::once(SERIES[selected])
        .chain(SECONDARY[selected])
        .filter_map(|i| history.series[i].summary.peak().map(|p| p.0))
        .fold(0.0_f64, f64::max);
    if matches!(selected, 2..=5) {
        let (divisor, unit) = rate_scale(peak);
        (divisor, (peak / divisor).max(1.0), unit)
    } else if selected <= 1 || selected >= 6 {
        (1.0, 100.0, "%")
    } else {
        (1.0, ((peak / 20.0).ceil() * 20.0).clamp(20.0, 100.0), "%")
    }
}

fn summary(history: &History, index: usize, label: &str, width: usize) -> String {
    let stats = &history.series[index].summary;
    let when = if width >= 70 {
        stats.peak().map_or_else(String::new, |(_, time)| {
            format!(" at {}", local_time(time, "%H:%M:%S"))
        })
    } else {
        String::new()
    };
    format!(
        "{label} avg {} · peak {}{when}",
        value(index, stats.mean()),
        value(index, stats.peak().map(|p| p.0))
    )
}

fn detail(history: &History, selected: usize) -> Option<String> {
    if selected == 1 || selected >= 6 {
        let memory = history.last_memory.as_ref()?;
        let size = |pair: Option<(u64, u64)>| {
            pair.map_or_else(
                || "N/A".into(),
                |(used, total)| {
                    format!(
                        "{:.2}/{:.2} GiB",
                        used as f64 / 1_073_741_824.0,
                        total as f64 / 1_073_741_824.0
                    )
                },
            )
        };
        if selected >= 6 {
            return Some(format!(
                "Last: VRAM {} · {}",
                size(memory.vram),
                value(10, memory.vram.map(|(u, t)| u as f64 / t as f64 * 100.0))
            ));
        }
        Some(format!(
            "Last: RAM {} · swap {}",
            size(memory.ram),
            size(memory.swap)
        ))
    } else if matches!(selected, 2..=5) {
        let total = |index: usize| {
            let transfer = &history.series[index].transfer;
            transfer.bytes().map_or_else(
                || "N/A".into(),
                |bytes| {
                    format!(
                        "~{}/{}s",
                        crate::cli::format_byte_rate(Some(bytes)).trim_end_matches("/s"),
                        transfer.seconds
                    )
                },
            )
        };
        Some(format!(
            "Observed: {} {}",
            LABELS[selected].0,
            total(SERIES[selected])
        ))
    } else {
        None
    }
}

/// Direction matches RX/TX; traffic is the mean, health rates are raw peaks.
pub(super) fn network_details(history: &History, tx: bool) -> Vec<String> {
    let direction = usize::from(tx);
    let mut lines = Vec::new();
    for (name, group) in lantern::model::NETWORK_GROUP_NAMES
        .iter()
        .zip(&history.network_groups)
    {
        if group.iter().all(|stat| stat.mean().is_none()) {
            continue;
        }
        let count = |i: usize| {
            group[i]
                .peak()
                .map(|(value, _)| value)
                .map_or_else(|| "N/A".into(), |v| format!("{v:.2}"))
        };
        lines.push(format!(
            "{name}: avg {} · err max {}/s · drop max {}/s",
            crate::cli::format_byte_rate(group[direction].mean()),
            count(2 + direction),
            count(4 + direction)
        ));
    }
    if lines.is_empty() {
        lines.push("Network groups: N/A in this history".into());
    }
    lines
}

pub(super) fn frame(
    history: &History,
    pages: &Pages,
    selected: usize,
    size: (u16, u16),
    events: &Events,
    selected_event: usize,
) -> Frame {
    let width = usize::from(size.0).saturating_sub(1);
    let height = usize::from(size.1);
    let mut frame = Frame::new();
    frame.colored(
        format!(
            "Lantern · {}",
            if pages.has_newer() {
                "HISTORY"
            } else {
                "FOLLOWING LATEST"
            }
        ),
        Color::Cyan,
    );
    if size.0 < 32 || height < 12 {
        frame.push("Resize terminal to at least 32 x 12");
        frame.push("Q quit");
        return frame.fit(width, height);
    }
    frame.push(format!(
        "{} → {}",
        local_time(history.start, "%m-%d %H:%M %z"),
        local_time(history.end - 1, "%m-%d %H:%M %z")
    ));
    frame.push(format!(
        "{} samples · saved data · local time",
        history.rows
    ));
    let event = events.items.get(selected_event);
    if height >= 16 {
        frame.colored(
            if events.items.is_empty() {
                "Events: 0 · no sustained threshold crossings".into()
            } else {
                format!(
                    "Events: {} · {}/{} · ←/→ browse",
                    events.total,
                    selected_event + 1,
                    events.items.len()
                )
            },
            Color::Yellow,
        );
        if let Some(event) = event {
            frame.colored(super::event_summary(event), Color::Yellow);
            frame.push(super::event_process(event));
            if height >= 24 {
                frame.push(super::event_rule(event));
            }
        }
    } else {
        frame.lines[2] = format!("{} samples · {} events", history.rows, events.total);
    }

    if height >= 32 {
        frame.push("   Metric          Primary avg     Secondary avg");
        for (index, title) in TITLES.iter().enumerate().take(metric_count(history)) {
            let marker = if selected == index { '>' } else { ' ' };
            let first = value(SERIES[index], history.series[SERIES[index]].summary.mean());
            let second = SECONDARY[index].map_or_else(
                || "—".into(),
                |i| value(i, history.series[i].summary.mean()),
            );
            frame.colored(
                format!("{marker}  {title:<12} {first:>13} {second:>15}"),
                COLORS[index],
            );
        }
    }
    let series_index = SERIES[selected];
    let first = &history.series[series_index];
    let second = SECONDARY[selected].map(|index| &history.series[index]);
    let peak = first.summary.peak();
    let second_peak = second.and_then(|series| series.summary.peak());
    let cpu = matches!(selected, 0 | 6);
    let (divisor, top, unit) = shared_scale(history, selected);
    let second_top = if cpu {
        temperature_top(second_peak.map_or(0.0, |p| p.0))
    } else {
        top * divisor
    };
    let matched = event.filter(|event| event_metric(event.kind) == selected);
    let marker = matched.and_then(|event| event_column(history, event));
    let title = if let Some(event) = matched {
        format!(
            "{} · ◆ event {}{}",
            TITLES[selected],
            local_time(event.peak_time, "%H:%M:%S"),
            if marker.is_none() {
                " (outside page)"
            } else {
                ""
            }
        )
    } else {
        format!(
            "{} · {}",
            TITLES[selected],
            if cpu {
                "use + temperature"
            } else {
                "mean curves"
            }
        )
    };
    frame.colored(title, COLORS[selected]);
    let legend = match selected {
        0 | 6 => "─ use % left · temp °C right".into(),
        1 => "─ RAM % · swap % (each capacity)".into(),
        2 | 3 => format!("─ {} ({unit}) · all interfaces", LABELS[selected].0),
        4 | 5 => format!("─ {} ({unit}) · mean curve", LABELS[selected].0),
        _ => "─ VRAM % · mean curve".into(),
    };
    let split = legend.chars().position(|c| c == '·');
    let legend_row = frame.lines.len();
    frame.colored(legend.clone(), COLORS[selected]);
    if second.is_some()
        && let Some(split) = split
    {
        frame.segment(
            legend_row,
            split,
            legend.chars().skip(split).collect(),
            SECOND_COLORS[selected],
        );
    }
    let detail = if height >= 16 {
        detail(history, selected)
    } else {
        None
    };
    let network = if matches!(selected, 2 | 3) && height >= 24 {
        network_details(history, selected == 3)
    } else {
        Vec::new()
    };
    let plot_height = height
        .saturating_sub(frame.lines.len() + 5 + usize::from(detail.is_some()) + network.len())
        .clamp(2, 16);
    if peak.is_some() || second_peak.is_some() {
        let grid = curve(first, top * divisor, plot_height, false);
        let other = second.map(|series| curve(series, second_top, plot_height, cpu));
        let marker_row = matched.filter(|_| marker.is_some()).map(|event| {
            let scale = if event.kind == Kind::Temperature {
                second_top
            } else {
                top * divisor
            };
            plot_height
                - 1
                - (event.peak / scale * (plot_height - 1) as f64)
                    .round()
                    .clamp(0.0, (plot_height - 1) as f64) as usize
        });
        for (row, cells) in grid.iter().enumerate() {
            let factor = (plot_height - row - 1) as f64 / (plot_height - 1) as f64;
            let axis = if peak.is_some() || !cpu {
                format!("{:>6.1} │", top * factor)
            } else {
                "   N/A │".into()
            };
            let mut text = axis.clone();
            let screen_row = frame.lines.len();
            frame.segment(screen_row, 0, axis, COLORS[selected]);
            for (x, cell) in cells.iter().enumerate() {
                let secondary = other.as_ref().is_some_and(|grid| grid[row][x] != 0);
                let (ch, color) = if marker == Some(x) && marker_row == Some(row) {
                    ('◆', EVENT_COLOR)
                } else if *cell != 0 && secondary {
                    ('×', Color::Magenta)
                } else if secondary {
                    ('·', SECOND_COLORS[selected])
                } else if *cell != 0 {
                    (glyph(*cell), COLORS[selected])
                } else if marker == Some(x) {
                    ('┊', EVENT_COLOR)
                } else {
                    (' ', Color::Reset)
                };
                text.push(ch);
                if ch != ' ' {
                    frame.segment(screen_row, MARGIN + x, ch.to_string(), color);
                }
            }
            if cpu {
                let axis = if second_peak.is_some() {
                    format!("│{:>6.1}", second_top * factor)
                } else {
                    "│   N/A".into()
                };
                frame.segment(
                    screen_row,
                    MARGIN + cells.len(),
                    axis.clone(),
                    SECOND_COLORS[selected],
                );
                text.push_str(&axis);
            }
            frame.push(text);
        }
        let mut axis = time_axis(history.start, history.end, first.buckets.len());
        if let Some(column) = marker {
            let mut chars: Vec<_> = axis.chars().collect();
            if chars[column] == ' ' {
                chars[column] = '▲';
            }
            axis = chars.into_iter().collect();
        }
        frame.push(format!("{}{}", " ".repeat(MARGIN), axis));
        frame.colored(
            summary(history, series_index, LABELS[selected].0, width),
            COLORS[selected],
        );
        if let Some(index) = SECONDARY[selected] {
            let text = if selected == 1
                && history.series[6].summary.mean().is_none()
                && history
                    .last_memory
                    .as_ref()
                    .is_some_and(|m| m.swap == Some((0, 0)))
            {
                "Swap disabled at last sample (0 GiB)".into()
            } else {
                summary(history, index, LABELS[selected].1, width)
            };
            frame.colored(text, SECOND_COLORS[selected]);
        } else if let Some((_, time)) = peak {
            frame.push(format!("Peak at {}", local_time(time, "%m-%d %H:%M:%S %z")));
        }
    } else {
        frame.push(if history.rows == 0 {
            "No measurements in this period"
        } else {
            "N/A · metrics unavailable"
        });
    }
    if let Some(detail) = detail {
        frame.push(detail);
    }
    for line in network {
        frame.push(line);
    }
    // Clip graph output before adding the two pinned control rows.
    frame.lines.resize(height - 2, String::new());
    frame.paints.retain(|p| p.row < height - 2);
    frame.push("←/→ events · ↑/↓ metric");
    frame.push("L/R time · Q quit");
    frame.fit(width, height)
}

pub(super) fn draw(output: &mut impl Write, frame: &Frame) -> io::Result<()> {
    let mut bytes = Vec::new();
    queue!(bytes, ResetColor, MoveTo(0, 0), Clear(ClearType::All))?;
    for (row, line) in frame.lines.iter().enumerate() {
        queue!(bytes, MoveTo(0, row as u16))?;
        write!(bytes, "{line}")?;
    }
    // Adjacent same-colour cells are emitted as one run, avoiding a cursor and
    // colour escape for every point along a line.
    let mut active: Option<(usize, usize, Color)> = None;
    for paint in &frame.paints {
        if paint.text.is_empty() {
            continue;
        }
        if active != Some((paint.row, paint.column, paint.color)) {
            queue!(
                bytes,
                MoveTo(paint.column as u16, paint.row as u16),
                SetForegroundColor(paint.color)
            )?;
        }
        write!(bytes, "{}", paint.text)?;
        active = Some((
            paint.row,
            paint.column + paint.text.chars().count(),
            paint.color,
        ));
    }
    queue!(bytes, ResetColor)?;
    output.write_all(&bytes)?;
    output.flush()
}
#[cfg(test)]
mod tests {
    use super::*;
    use lantern::model::SystemSample;

    #[test]
    fn frames_fit_small_normal_and_large_terminals_with_and_without_gpu() {
        for size in [
            (0, 0),
            (10, 4),
            (32, 12),
            (40, 20),
            (80, 24),
            (80, 32),
            (120, 40),
            (200, 80),
        ] {
            for gpu in [None, Some(50)] {
                let mut history = History::new(0, 100, plot_width(size.0)).unwrap();
                let mut sample = SystemSample::new(50);
                sample.cpu_percent = Some(10.0);
                sample.gpu_utilisation = gpu;
                history.add(&sample);
                for (selected, title) in TITLES.iter().enumerate().take(metric_count(&history)) {
                    let lines = frame(
                        &history,
                        &Pages::new(0, 99).unwrap(),
                        selected,
                        size,
                        &Events::default(),
                        0,
                    );
                    assert!(lines.len() <= usize::from(size.1));
                    assert!(
                        lines
                            .iter()
                            .all(|line| line.chars().count() < usize::from(size.0).max(1))
                    );
                    if size.0 >= 32 && size.1 >= 12 {
                        assert!(lines.last().unwrap().contains("Q quit"));
                        if size.1 >= 32 {
                            assert!(lines.iter().any(|line| line.contains("Primary avg")));
                        }
                        assert!(lines.iter().any(|line| line.contains(title)));
                    }
                }
            }
        }
    }

    #[test]
    fn event_details_remain_above_graph_and_fit_with_long_names() {
        let mut history = History::new(0, 40, 30).unwrap();
        let mut analyzer = lantern::events::Analyzer::new(0, 40);
        for time in (0..=30).step_by(5) {
            let mut sample = SystemSample::new(time);
            sample.cpu_percent = Some(99.0);
            sample.top_cpu = Some(lantern::model::ProcessUsage {
                pid: 42,
                name: "very-long-worker-process-name".into(),
                start_ticks: 0,
                usage: 50.0,
            });
            history.add(&sample);
            analyzer.add(&sample);
        }
        let events = analyzer.finish();
        for size in [(32, 12), (40, 16), (80, 24), (120, 40)] {
            let lines = frame(&history, &Pages::new(0, 39).unwrap(), 0, size, &events, 0);
            assert_eq!(lines.len(), usize::from(size.1));
            assert!(
                lines
                    .iter()
                    .all(|line| line.chars().count() < usize::from(size.0))
            );
            assert!(lines.last().unwrap().contains("Q quit"));
            if size.1 >= 16 {
                let event = lines
                    .iter()
                    .position(|line| line.contains("High CPU"))
                    .unwrap();
                let graph = lines
                    .iter()
                    .position(|line| line.starts_with("CPU ·"))
                    .unwrap();
                assert!(event < graph);
            }
        }
    }

    #[test]
    fn temperature_event_and_graph_share_raw_peak_above_one_hundred() {
        let mut history = History::new(0, 400, 8).unwrap();
        let mut analyzer = lantern::events::Analyzer::new(0, 40);
        for time in (0..=30).step_by(5) {
            let mut sample = SystemSample::new(time);
            sample.cpu_percent = Some(20.0);
            sample.cpu_temperature = Some(if time == 15 { 101.6 } else { 81.0 });
            history.add(&sample);
            analyzer.add(&sample);
        }
        let events = analyzer.finish();
        assert_eq!(events.items.len(), 1);
        assert_eq!(events.items[0].peak, 101.6);
        assert_eq!(history.series[1].summary.peak(), Some((101.6, 15)));
        let frame = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            0,
            (100, 24),
            &events,
            0,
        )
        .join("\n");
        assert!(frame.contains("use % left · temp °C right"));
        assert!(frame.contains("│ 110.0"));
        assert!(frame.contains("peak 101.6 °C"));
        let top = temperature_top(101.6);
        let peaks = curve(&history.series[1], top, 12, true);
        let means = curve(&history.series[1], top, 12, false);
        assert_ne!(
            peaks, means,
            "temperature peaks must survive bucket averaging"
        );
        assert!(
            peaks[1][0] != 0,
            "101.6 must reach its position on a 110-degree axis"
        );
    }

    #[test]
    fn every_event_has_a_graph_with_an_exact_peak_marker_and_clipped_colours() {
        let mut history = History::new(0, 121, plot_width(80)).unwrap();
        let mut analyzer = lantern::events::Analyzer::new(0, 121);
        for time in (0..=120).step_by(5) {
            let mut sample = SystemSample::new(time);
            sample.cpu_percent = Some(95.0);
            sample.cpu_temperature = Some(101.6);
            sample.memory_used = Some(95);
            sample.memory_total = Some(100);
            sample.swap_used = Some(80);
            sample.swap_total = Some(100);
            sample.gpu_utilisation = Some(99);
            let rate = if time < 60 { 1000.0 } else { 20_000_000.0 };
            sample.network_rx_rate = Some(rate);
            sample.network_tx_rate = Some(rate);
            sample.disk_read_rate = Some(rate);
            sample.disk_write_rate = Some(rate);
            history.add(&sample);
            analyzer.add(&sample);
        }
        let events = analyzer.finish();
        assert_eq!(events.items.len(), 9);
        for (index, event) in events.items.iter().enumerate() {
            let metric = event_metric(event.kind);
            let frame = frame(
                &history,
                &Pages::new(0, 120).unwrap(),
                metric,
                (80, 24),
                &events,
                index,
            );
            let expected_column = MARGIN + event_column(&history, event).unwrap();
            assert!(
                frame.paints.iter().any(|p| p.column == expected_column
                    && p.text == "◆"
                    && p.color == EVENT_COLOR)
            );
            assert!(
                frame
                    .lines
                    .iter()
                    .any(|line| line.starts_with(&format!("{} · ◆ event", TITLES[metric])))
            );
            for paint in &frame.paints {
                assert!(paint.row < 24);
                assert!(paint.column + paint.text.chars().count() < 80);
            }
            let mut ansi = Vec::new();
            draw(&mut ansi, &frame).unwrap();
            assert!(!ansi.contains(&b'\n'));
            assert!(!frame.join("").contains('\x1b'));
        }
    }

    #[test]
    fn combined_cpu_handles_missing_readings_and_distinguishes_curve_styles() {
        for cpu in [None, Some(30.0)] {
            for temperature in [None, Some(101.6)] {
                let mut history = History::new(0, 40, plot_width(80)).unwrap();
                let mut sample = SystemSample::new(5);
                sample.cpu_percent = cpu;
                sample.cpu_temperature = temperature;
                history.add(&sample);
                let f = frame(
                    &history,
                    &Pages::new(0, 39).unwrap(),
                    0,
                    (80, 24),
                    &Events::default(),
                    0,
                );
                if temperature.is_some() {
                    assert!(
                        f.paints
                            .iter()
                            .any(|p| p.text == "·" && p.color == TEMP_COLOR)
                    );
                    assert!(f.join("\n").contains("101.6 °C"));
                }
                if cpu.is_some() {
                    assert!(f.join("\n").contains("30.0 %"));
                }
                assert!(!f.paints.iter().any(|p| p.text == "◆"));
            }
        }
    }

    #[test]
    fn gpu_temperature_and_vram_work_without_utilisation_and_preserve_high_peaks() {
        let mut h = History::new(0, 100, plot_width(100)).unwrap();
        let mut sample = SystemSample::new(5);
        sample.gpu_temperature = Some(105);
        sample.gpu_memory_used = Some(2 * 1024_u64.pow(3));
        sample.gpu_memory_total = Some(8 * 1024_u64.pow(3));
        h.add(&sample);
        assert_eq!(metric_count(&h), 8);
        let gpu = frame(
            &h,
            &Pages::new(0, 99).unwrap(),
            6,
            (100, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(gpu.contains("105.0 °C") && gpu.contains("│ 110.0"), "{gpu}");
        assert!(gpu.contains("VRAM 2.00/8.00 GiB · 25.0 %"));
        assert!(gpu.contains("GPU avg N/A"));
        let vram = frame(
            &h,
            &Pages::new(0, 99).unwrap(),
            7,
            (100, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(vram.contains("VRAM avg 25.0 %"));
        h.add(&SystemSample::new(10));
        assert!(detail(&h, 6).unwrap().contains("VRAM N/A"));
    }

    #[test]
    fn rare_packet_errors_remain_visible_across_a_day() {
        let mut h = History::new(0, 86_400, plot_width(80)).unwrap();
        for time in (0..86_400).step_by(5) {
            let mut s = SystemSample::new(time);
            s.network_groups[0] = [Some(0.0); 6];
            if time == 50 {
                s.network_groups[0][2] = Some(0.2);
            }
            h.add(&s);
        }
        assert!(network_details(&h, false)[0].contains("err max 0.20/s"));
        assert!(network_details(&h, true)[0].contains("err max 0.00/s"));
    }

    #[test]
    fn network_details_fit_with_events_and_keep_directions_and_missing_data_distinct() {
        let mut h = History::new(0, 40, plot_width(80)).unwrap();
        let mut a = lantern::events::Analyzer::new(0, 40);
        for time in (0..=30).step_by(5) {
            let mut s = SystemSample::new(time);
            s.cpu_percent = Some(99.0);
            s.network_rx_rate = Some(1_000.0);
            s.network_tx_rate = Some(2_000.0);
            s.network_groups = [[
                Some(250.0),
                Some(500.0),
                Some(1.0),
                Some(2.0),
                Some(3.0),
                None,
            ]; 4];
            h.add(&s);
            a.add(&s);
        }
        let events = a.finish();
        for (selected, rate, errors, drops) in [
            (2, "250.0 B/s", "1.00", "3.00"),
            (3, "500.0 B/s", "2.00", "N/A"),
        ] {
            let f = frame(
                &h,
                &Pages::new(0, 39).unwrap(),
                selected,
                (80, 24),
                &events,
                0,
            )
            .join("\n");
            for name in lantern::model::NETWORK_GROUP_NAMES {
                assert!(
                    f.contains(&format!(
                        "{name}: avg {rate} · err max {errors}/s · drop max {drops}/s"
                    )),
                    "{f}"
                );
            }
            assert!(f.contains("Q quit"));
        }
    }

    #[test]
    fn rate_graphs_are_separate_with_independent_scales_and_totals() {
        let mut history = History::new(0, 40, plot_width(80)).unwrap();
        for time in [0, 5, 10] {
            let mut sample = SystemSample::new(time);
            sample.network_rx_rate = Some(1_000_000.0);
            sample.network_tx_rate = Some(10_000_000.0);
            sample.disk_write_rate = Some(5_000_000.0);
            history.add(&sample);
        }
        assert_eq!(shared_scale(&history, 2), (1_000_000.0, 1.0, "MB/s"));
        assert_eq!(shared_scale(&history, 3), (1_000_000.0, 10.0, "MB/s"));
        let rx = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            2,
            (100, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(rx.contains("RX (MB/s)"));
        assert!(rx.contains("RX ~10.0 MB/10s"));
        assert!(!rx.contains("TX"));
        let tx = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            3,
            (100, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(tx.contains("TX ~100.0 MB/10s"));
        assert!(!tx.contains("RX"));
        let disk = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            5,
            (80, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(disk.contains("Write avg 5.0 MB/s"));
        assert!(!disk.contains("Read avg"));
    }

    #[test]
    fn memory_pair_shows_capacity_and_distinguishes_disabled_swap_from_missing_data() {
        let mut history = History::new(0, 40, plot_width(80)).unwrap();
        let mut sample = SystemSample::new(5);
        sample.memory_used = Some(2 * 1024_u64.pow(3));
        sample.memory_total = Some(4 * 1024_u64.pow(3));
        sample.swap_used = Some(0);
        sample.swap_total = Some(0);
        history.add(&sample);
        let text = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            1,
            (80, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(text.contains("RAM 2.00/4.00 GiB"));
        assert!(text.contains("Swap disabled"));
        assert_eq!(shared_scale(&history, 1), (1.0, 100.0, "%"));
        history.add(&SystemSample::new(10));
        let text = frame(
            &history,
            &Pages::new(0, 39).unwrap(),
            1,
            (80, 24),
            &Events::default(),
            0,
        )
        .join("\n");
        assert!(text.contains("Last: RAM N/A · swap N/A"));
        assert!(!text.contains("Swap disabled"));
    }

    #[test]
    fn draw_positions_rows_without_newlines_or_scrolling() {
        let mut output = Vec::new();
        let mut frame = Frame::new();
        frame.colored("Lantern", Color::Cyan);
        frame.push("> CPU");
        draw(&mut output, &frame).unwrap();
        assert!(!output.contains(&b'\n'));
        assert!(!output.contains(&b'\r'));
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("\x1b[1;1H"));
        assert!(text.contains("\x1b[2;1H"));
    }
}
