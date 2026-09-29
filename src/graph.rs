use std::{
    error::Error,
    io::{self, IsTerminal, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Local};
mod screen;

use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, terminal,
};
use lantern::{
    events::{Analyzer, Event as ResourceEvent, Events, Kind},
    history::{History, Series},
    storage::{Database, default_db_path},
};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    flag,
    low_level::unregister,
};

const DAY: i64 = 24 * 60 * 60;
const EMPTY: &str = "No historical Lantern data is available yet.\nStart the collector with:\n\n    lantern --collect\n";
const MARGIN: usize = 8;
const HEIGHT: usize = 5;

pub fn run(output: &mut impl Write) -> Result<(), Box<dyn Error>> {
    let Some(database) = Database::open_existing(default_db_path()?)? else {
        output.write_all(EMPTY.as_bytes())?;
        return Ok(());
    };
    let Some((first, last)) = database.timestamp_bounds()? else {
        output.write_all(EMPTY.as_bytes())?;
        return Ok(());
    };
    let pages = Pages::new(first, last)?;
    if io::stdin().is_terminal() && io::stdout().is_terminal() {
        run_interactive(output, &database, pages)
    } else {
        let (history, events) = load_page(&database, &pages, columns(terminal_width()))?;
        render(output, &history, &events)?;
        Ok(())
    }
}

fn load_page(
    database: &Database,
    pages: &Pages,
    width: usize,
) -> Result<(History, Events), Box<dyn Error>> {
    let lower = pages.start();
    let start = database
        .first_timestamp_in_range(lower, pages.end)?
        .unwrap_or(lower);
    let mut history = History::new(start, pages.end, width)
        .ok_or_else(|| io::Error::other("cannot determine history range"))?;
    let mut analyzer = Analyzer::new(start, pages.end);
    database.visit_range(start.saturating_sub(300), pages.end, |sample| {
        history.add(&sample);
        analyzer.add(&sample);
    })?;
    Ok((history, analyzer.finish()))
}

fn run_interactive(
    output: &mut impl Write,
    database: &Database,
    mut pages: Pages,
) -> Result<(), Box<dyn Error>> {
    let input = TerminalInput::new(output)?;
    let mut size = terminal::size()?;
    let (mut history, mut events) = load_page(database, &pages, screen::plot_width(size.0))?;
    let mut selected_event = 0;
    let mut selected = events
        .items
        .first()
        .map_or(0, |event| screen::event_metric(event.kind));
    let mut linked = true;
    let mut next_refresh = Instant::now() + Duration::from_secs(5);
    loop {
        selected = selected.min(screen::metric_count(&history) - 1);
        selected_event = selected_event.min(events.items.len().saturating_sub(1));
        screen::draw(
            output,
            &screen::frame(&history, &pages, selected, size, &events, selected_event),
        )?;
        let mut changed_period = false;
        let event_key = events
            .items
            .get(selected_event)
            .map(|event| (event.kind, event.start));
        let reload = loop {
            match input.read(next_refresh)? {
                Action::Quit => return Ok(()),
                Action::NextEvent if !events.items.is_empty() => {
                    selected_event = (selected_event + 1) % events.items.len();
                    selected = screen::event_metric(events.items[selected_event].kind);
                    linked = true;
                    break false;
                }
                Action::PreviousEvent if !events.items.is_empty() => {
                    selected_event = (selected_event + events.items.len() - 1) % events.items.len();
                    selected = screen::event_metric(events.items[selected_event].kind);
                    linked = true;
                    break false;
                }
                Action::Older if pages.older() => {
                    changed_period = true;
                    break true;
                }
                Action::Newer if pages.newer() => {
                    changed_period = true;
                    break true;
                }
                Action::NextMetric => {
                    linked = false;
                    selected = (selected + 1) % screen::metric_count(&history);
                    break false;
                }
                Action::PreviousMetric => {
                    linked = false;
                    selected = (selected + screen::metric_count(&history) - 1)
                        % screen::metric_count(&history);
                    break false;
                }
                Action::Resize => {
                    size = terminal::size()?;
                    break true;
                }
                Action::Refresh => {
                    next_refresh = Instant::now() + Duration::from_secs(5);
                    if let Some((first, last)) = database.timestamp_bounds()? {
                        if pages.refresh(first, last)? {
                            break true;
                        }
                    } else if history.rows > 0 {
                        break true;
                    }
                }
                _ => {}
            }
        };
        // No transaction or read lock remains held while waiting for input.
        if reload {
            (history, events) = load_page(database, &pages, screen::plot_width(size.0))?;
            selected_event = if changed_period {
                0
            } else {
                events
                    .items
                    .iter()
                    .position(|event| Some((event.kind, event.start)) == event_key)
                    .unwrap_or(0)
            };
            if changed_period {
                linked = true;
            }
            if linked {
                selected = events
                    .items
                    .get(selected_event)
                    .map_or(0, |event| screen::event_metric(event.kind));
            }
        }
    }
}

struct Pages {
    first: i64,
    latest_end: i64,
    end: i64,
}

impl Pages {
    fn new(first: i64, last: i64) -> io::Result<Self> {
        let end = last
            .checked_add(1)
            .filter(|_| first <= last)
            .ok_or_else(|| io::Error::other("history timestamps are out of range"))?;
        Ok(Self {
            first,
            latest_end: end,
            end,
        })
    }
    fn refresh(&mut self, first: i64, last: i64) -> io::Result<bool> {
        let updated = Self::new(first, last)?;
        if self.first == first && self.latest_end == updated.latest_end {
            return Ok(false);
        }
        if !self.has_newer() {
            self.end = updated.latest_end;
        } else if self.end <= first {
            self.end = first.saturating_add(DAY).min(updated.latest_end);
        } else {
            self.end = self.end.min(updated.latest_end);
        }
        self.first = first;
        self.latest_end = updated.latest_end;
        Ok(true)
    }
    fn start(&self) -> i64 {
        self.end.saturating_sub(DAY).max(self.first)
    }
    fn has_older(&self) -> bool {
        self.start() > self.first
    }
    fn has_newer(&self) -> bool {
        self.end < self.latest_end
    }
    fn older(&mut self) -> bool {
        if !self.has_older() {
            return false;
        }
        self.end = self.end.saturating_sub(DAY);
        true
    }
    fn newer(&mut self) -> bool {
        if !self.has_newer() {
            return false;
        }
        self.end = self.end.saturating_add(DAY).min(self.latest_end);
        true
    }
}

#[derive(Debug, PartialEq)]
enum Action {
    NextEvent,
    PreviousEvent,
    NextMetric,
    PreviousMetric,
    Refresh,
    Older,
    Newer,
    Resize,
    Quit,
    Ignore,
}

/// Restore the terminal on exit, including errors and termination signals.
struct TerminalInput {
    stopped: Arc<AtomicBool>,
    handlers: Vec<SigId>,
    raw: bool,
    screen: bool,
}

impl TerminalInput {
    fn new(output: &mut impl Write) -> io::Result<Self> {
        let mut input = Self {
            stopped: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
            raw: false,
            screen: false,
        };
        for signal in [SIGINT, SIGTERM] {
            input
                .handlers
                .push(flag::register(signal, Arc::clone(&input.stopped))?);
        }
        terminal::enable_raw_mode()?;
        input.raw = true;
        // Mark before writing so a partially failed setup is also restored.
        input.screen = true;
        execute!(output, terminal::EnterAlternateScreen, Hide)?;
        Ok(input)
    }
    fn read(&self, deadline: Instant) -> io::Result<Action> {
        loop {
            if self.stopped.load(Ordering::Relaxed) {
                return Ok(Action::Quit);
            }
            // Blocks between events; also lets SIGTERM restore terminal settings.
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Action::Refresh);
            }
            if !event::poll(remaining.min(Duration::from_secs(1)))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    return Ok(match key.code {
                        KeyCode::Down => Action::NextMetric,
                        KeyCode::Up => Action::PreviousMetric,
                        KeyCode::Right => Action::NextEvent,
                        KeyCode::Left => Action::PreviousEvent,
                        KeyCode::Char('l' | 'L') => Action::Older,
                        KeyCode::Char('r' | 'R') => Action::Newer,
                        KeyCode::Char('q' | 'Q') => Action::Quit,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Action::Quit
                        }
                        _ => Action::Ignore,
                    });
                }
                Event::Resize(_, _) => return Ok(Action::Resize),
                _ => {}
            }
        }
    }
}

impl Drop for TerminalInput {
    fn drop(&mut self) {
        if self.screen {
            let _ = execute!(
                io::stdout(),
                crossterm::style::ResetColor,
                Show,
                terminal::LeaveAlternateScreen
            );
        }
        if self.raw {
            let _ = terminal::disable_raw_mode();
        }
        for handler in self.handlers.drain(..) {
            unregister(handler);
        }
    }
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|width| *width > 0)
        .or_else(|| terminal_size::terminal_size().map(|(width, _)| usize::from(width.0)))
        .unwrap_or(80)
}

fn columns(width: usize) -> usize {
    width.saturating_sub(MARGIN).clamp(8, 160)
}

fn local_time(timestamp: i64, format: &str) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(|time| time.with_timezone(&Local).format(format).to_string())
        .unwrap_or_else(|| "N/A".into())
}

fn event_value(kind: Kind, value: f64) -> String {
    if kind.is_rate() {
        crate::cli::format_byte_rate(Some(value))
    } else if kind == Kind::Temperature {
        format!("{value:.1} °C")
    } else {
        format!("{value:.1} %")
    }
}

fn event_summary(event: &ResourceEvent) -> String {
    format!(
        "{} {} · {} · peak {}",
        local_time(event.start, "%m-%d %H:%M:%S"),
        event.kind.label(),
        format_duration(event.end.saturating_sub(event.start)),
        event_value(event.kind, event.peak)
    )
}

fn format_duration(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn event_process(event: &ResourceEvent) -> String {
    let Some(process) = &event.process else {
        return if matches!(event.kind, Kind::Cpu | Kind::Memory) {
            "Process unknown · no evidence at peak".into()
        } else {
            "Process unknown · attribution unavailable for this metric".into()
        };
    };
    let usage = if event.kind == Kind::Cpu {
        format!("{:.1}% machine CPU", process.usage)
    } else {
        format!("{:.1} MiB RSS", process.usage / (1024.0 * 1024.0))
    };
    // Sanitize again: history may have been imported or modified externally.
    let name: String = process
        .name
        .chars()
        .take(32)
        .map(|c| {
            if c.is_ascii() && !c.is_control() {
                c
            } else {
                '?'
            }
        })
        .collect();
    format!(
        "Top at peak: {name} ({}) {usage} · possible contributor",
        process.pid
    )
}

fn event_rule(event: &ResourceEvent) -> String {
    format!(
        "Rule >= {} for 30s{} · peak {}",
        event_value(event.kind, event.threshold),
        if event.kind.is_rate() {
            " (6x prior mean, floor)"
        } else {
            ""
        },
        local_time(event.peak_time, "%H:%M:%S")
    )
}

fn render(output: &mut impl Write, history: &History, events: &Events) -> io::Result<()> {
    writeln!(output, "Lantern · History")?;
    writeln!(
        output,
        "From {}",
        local_time(history.start, "%Y-%m-%d %H:%M:%S %z")
    )?;
    writeln!(
        output,
        "To   {}",
        local_time(history.end - 1, "%Y-%m-%d %H:%M:%S %z")
    )?;
    if history.rows == 0 {
        return writeln!(output, "No measurements in this period.");
    }
    writeln!(output, "{} samples · local time", history.rows)?;
    writeln!(output, "Events: {} · analysed on view", events.total)?;
    for event in &events.items {
        for line in [
            event_summary(event),
            event_process(event),
            event_rule(event),
        ] {
            writeln!(
                output,
                "{}",
                line.chars().take(terminal_width()).collect::<String>()
            )?;
        }
    }
    if events.total > events.items.len() {
        writeln!(output, "Showing latest {} events", events.items.len())?;
    }
    writeln!(output, "Curves: mean; temperatures: peak")?;
    writeln!(output, "Gaps: no data")?;
    for (index, (title, series)) in [
        "CPU %",
        "CPU temp °C",
        "Memory %",
        "Network RX",
        "Network TX",
        "GPU %",
        "Swap %",
        "Disk read",
        "Disk write",
        "GPU temp °C",
        "VRAM %",
    ]
    .into_iter()
    .zip(&history.series)
    .enumerate()
    {
        if matches!(index, 5 | 9 | 10) && series.summary.mean().is_none() {
            continue;
        }
        render_series(
            output,
            title,
            series,
            if matches!(index, 1 | 9) {
                PlotUnit::Temperature
            } else if matches!(index, 3 | 4 | 7 | 8) {
                PlotUnit::Rate
            } else {
                PlotUnit::Percent
            },
            history.start,
            history.end,
        )?;
        if matches!(index, 3 | 4) {
            for line in screen::network_details(history, index == 4) {
                writeln!(output, "{line}")?;
            }
        }
    }
    Ok(())
}

enum PlotUnit {
    Percent,
    Temperature,
    Rate,
}

fn temperature_top(peak: f64) -> f64 {
    // Temperatures above 100 °C are valid readings, not invalid percentages.
    ((peak / 10.0).ceil() * 10.0).max(10.0)
}

fn render_series(
    output: &mut impl Write,
    title: &str,
    series: &Series,
    units: PlotUnit,
    start: i64,
    end: i64,
) -> io::Result<()> {
    let Some((peak, peak_time)) = series.summary.peak() else {
        return writeln!(output, "\n{title} · N/A");
    };
    let rate = matches!(units, PlotUnit::Rate);
    let temperature = matches!(units, PlotUnit::Temperature);
    let (divisor, unit) = match units {
        PlotUnit::Rate => rate_scale(peak),
        PlotUnit::Temperature => (1.0, "°C"),
        PlotUnit::Percent => (1.0, "%"),
    };
    let top = if rate {
        (peak / divisor).max(1.0)
    } else if temperature {
        temperature_top(peak)
    } else {
        ((peak / 20.0).ceil() * 20.0).clamp(20.0, 100.0)
    };
    if rate {
        writeln!(output, "\n{title} ({unit})")?;
    } else {
        writeln!(output, "\n{title}")?;
    }
    let curve = curve(series, divisor * top, HEIGHT, temperature);
    for (row, cells) in curve.iter().enumerate() {
        let label = top * (HEIGHT - 1 - row) as f64 / (HEIGHT - 1) as f64;
        if rate {
            write!(output, "{label:>6.1} │")?;
        } else {
            write!(output, "{label:>6.0} │")?;
        }
        for cell in cells {
            write!(output, "{}", glyph(*cell))?;
        }
        writeln!(output)?;
    }
    writeln!(
        output,
        "        {}",
        time_axis(start, end, series.buckets.len())
    )?;
    let value = |value: Option<f64>| {
        if rate {
            crate::cli::format_byte_rate(value)
        } else {
            value.map_or_else(|| "N/A".into(), |value| format!("{value:.1} {unit}"))
        }
    };
    let average = format!("avg {}", value(series.summary.mean()));
    let peak = format!("peak {}", value(Some(peak)));
    let when = local_time(peak_time, "%m-%d %H:%M:%S");
    let summary = format!("{average} · {peak} at {when}");
    if summary.chars().count() <= series.buckets.len() + MARGIN {
        writeln!(output, "{summary}")
    } else {
        writeln!(output, "{average} · {peak}")?;
        writeln!(output, "at {when}")
    }
}

const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const UP: u8 = 4;
const DOWN: u8 = 8;
const POINT: u8 = 16;

fn curve(series: &Series, scale: f64, height: usize, peaks: bool) -> Vec<Vec<u8>> {
    let mut grid = vec![vec![0; series.buckets.len()]; height];
    let mut previous: Option<(usize, usize, i64)> = None;
    for (x, bucket) in series.buckets.iter().enumerate() {
        let reading = if peaks {
            bucket.peak().map(|(value, _)| value)
        } else {
            bucket.mean()
        };
        let Some((mean, (first, last))) = reading.zip(bucket.timestamps()) else {
            continue;
        };
        let y = height
            - 1
            - (mean / scale * (height - 1) as f64)
                .round()
                .clamp(0.0, (height - 1) as f64) as usize;
        grid[y][x] |= POINT;
        if let Some((px, py, time)) = previous {
            // Join normal five-second readings even at high zoom. Preserve
            // empty buckets caused by missing readings or collection pauses.
            let close_in_time = first
                .checked_sub(time)
                .is_some_and(|gap| (0..=7).contains(&gap));
            if x == px + 1 || close_in_time {
                let mut row = py;
                for column in px + 1..=x {
                    let next_row = (py as f64
                        + (y as f64 - py as f64) * (column - px) as f64 / (x - px) as f64)
                        .round() as usize;
                    grid[row][column - 1] |= RIGHT;
                    grid[row][column] |= LEFT;
                    while row != next_row {
                        if row > next_row {
                            grid[row][column] |= UP;
                            row -= 1;
                            grid[row][column] |= DOWN;
                        } else {
                            grid[row][column] |= DOWN;
                            row += 1;
                            grid[row][column] |= UP;
                        }
                    }
                }
            }
        }
        previous = Some((x, y, last));
    }
    grid
}

fn glyph(cell: u8) -> char {
    match cell & 15 {
        0 if cell == 0 => ' ',
        0 => '•',
        1..=3 => '─',
        4 | 8 | 12 => '│',
        5 => '╯',
        6 => '╰',
        9 => '╮',
        10 => '╭',
        7 => '┴',
        11 => '┬',
        13 => '┤',
        14 => '├',
        _ => '┼',
    }
}

fn rate_scale(peak: f64) -> (f64, &'static str) {
    let mut divisor = 1.0;
    let units = ["B/s", "kB/s", "MB/s", "GB/s", "TB/s", "PB/s", "EB/s"];
    let mut index = 0;
    while peak / divisor >= 1000.0 && index < units.len() - 1 {
        divisor *= 1000.0;
        index += 1;
    }
    if peak / divisor >= 10000.0 {
        (peak, "fraction of peak")
    } else {
        (divisor, units[index])
    }
}

fn time_axis(start: i64, end: i64, width: usize) -> String {
    let mut axis = vec![b' '; width];
    let seconds = i128::from(end) - i128::from(start) < 600;
    let label_width = if seconds { 8 } else { 5 };
    let ticks = (width / (label_width + 3)).clamp(1, 5);
    for index in 0..ticks {
        let position = if ticks == 1 {
            0
        } else {
            index * (width - 1) / (ticks - 1)
        };
        let timestamp = i128::from(start)
            + (i128::from(end) - 1 - i128::from(start)) * position as i128 / (width - 1) as i128;
        let label = local_time(timestamp as i64, if seconds { "%H:%M:%S" } else { "%H:%M" });
        let label_start = position
            .saturating_sub(label_width / 2)
            .min(width - label_width);
        for (slot, byte) in axis[label_start..].iter_mut().zip(label.bytes()) {
            *slot = byte;
        }
    }
    String::from_utf8_lossy(&axis).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lantern::model::SystemSample;

    fn sample(timestamp: i64, percent: f64) -> SystemSample {
        let mut sample = SystemSample::new(timestamp);
        sample.cpu_percent = Some(percent);
        sample
    }

    #[test]
    fn pages_anchor_to_last_measurement_and_stop_at_history_limits() {
        let mut pages = Pages::new(100, 2 * DAY + 200).unwrap();
        assert_eq!(pages.end, 2 * DAY + 201);
        assert!(!pages.newer());
        assert!(pages.older());
        assert_eq!(pages.end, DAY + 201);
        assert!(pages.older());
        assert_eq!(pages.start(), 100);
        assert_eq!(pages.end, 201);
        assert!(!pages.older());
        assert!(pages.newer());
        assert!(pages.newer());
        assert_eq!(pages.end, pages.latest_end);
        assert!(!pages.newer());
    }

    #[test]
    fn single_measurement_has_a_valid_nonzero_range() {
        let pages = Pages::new(123, 123).unwrap();
        assert_eq!((pages.start(), pages.end), (123, 124));
        assert!(!pages.has_older());
        assert!(Pages::new(1, 0).is_err());
        assert!(Pages::new(0, i64::MAX).is_err());
    }

    #[test]
    fn refresh_follows_latest_but_keeps_older_views_stationary() {
        let mut pages = Pages::new(0, 3 * DAY).unwrap();
        assert!(pages.refresh(0, 3 * DAY + 30).unwrap());
        assert_eq!(pages.end, 3 * DAY + 31);
        assert!(pages.older());
        let older_end = pages.end;
        assert!(pages.refresh(0, 3 * DAY + 60).unwrap());
        assert_eq!(pages.end, older_end);
        assert!(!pages.refresh(0, 3 * DAY + 60).unwrap());
        assert!(pages.refresh(older_end + 1, 3 * DAY + 60).unwrap());
        assert!(pages.start() >= pages.first);
        assert!(pages.end > pages.first);
        assert!(pages.end <= pages.latest_end);
    }

    #[test]
    fn empty_page_keeps_its_dates_for_navigation() {
        let history = History::new(0, DAY, 8).unwrap();
        let mut output = Vec::new();
        render(&mut output, &history, &Events::default()).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("No measurements in this period"));
        assert!(text.contains("From "));
    }

    #[test]
    fn narrow_graphs_show_zero_and_missing_metrics_without_bars() {
        let mut history = History::new(0, 61, columns(40)).unwrap();
        for time in (0..=60).step_by(5) {
            history.add(&sample(time, 0.0));
        }
        let mut output = Vec::new();
        render(&mut output, &history, &Events::default()).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("     0 │──"));
        assert!(text.contains("Memory % · N/A"));
        assert!(text.contains("avg 0.0 %"));
        assert!(!text.contains("GPU %"));
        assert!(!text.contains('█'));
        assert!(!text.contains('\x1b'));
        assert!(
            text.lines().all(|line| line.chars().count() <= 40),
            "{text}"
        );
    }

    #[test]
    fn curves_join_frequent_samples_but_leave_collection_gaps_blank() {
        let mut history = History::new(0, 101, 100).unwrap();
        for (time, value) in [(0, 0.0), (5, 100.0), (90, 50.0), (95, 50.0)] {
            history.add(&sample(time, value));
        }
        let grid = curve(&history.series[0], 100.0, HEIGHT, false);
        assert!(grid.iter().any(|row| row[2] != 0));
        assert!(grid.iter().all(|row| row[50] == 0));
        assert!(grid.iter().any(|row| row[92] != 0));
    }

    #[test]
    fn curve_has_no_filled_area_and_summary_keeps_raw_peak() {
        let mut history = History::new(0, 100, 8).unwrap();
        for value in [0.0, 0.0, 0.0, 100.0] {
            history.add(&sample(1, value));
        }
        let grid = curve(&history.series[0], 100.0, HEIGHT, false);
        assert_eq!(grid.iter().flatten().filter(|cell| **cell != 0).count(), 1);
        let mut output = Vec::new();
        render(&mut output, &history, &Events::default()).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("avg 25.0 %"));
        assert!(text.contains("peak 100.0 %"));
    }

    #[test]
    fn terminal_dimensions_and_time_labels_are_bounded() {
        assert_eq!(columns(0), 8);
        assert_eq!(columns(80), 72);
        assert_eq!(columns(usize::MAX), 160);
        assert_eq!(time_axis(0, DAY, 8).len(), 8);
        assert_eq!(time_axis(0, 61, 8).len(), 8);
        assert_eq!(time_axis(0, 61, 160).len(), 160);
        assert_eq!(local_time(i64::MAX, "%H:%M"), "N/A");
    }
}
