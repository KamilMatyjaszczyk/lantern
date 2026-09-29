# Technical reference

Detailed behavior, data sources, event rules, storage, and development notes for
Lantern 1.0.1. See the [README](../README.md) for installation and everyday use.

## Quick status check

Run `lantern --status` independently of the graphs. It prints the database path
and size, first/last saved timestamps, sample age, and detected collector PIDs.
It reads the configured database without creating or migrating it, drawing graphs,
collecting metrics, or analysing events. There is no additional background work.

Collector detection checks visible `/proc` entries for a Lantern `--collect`
process holding the same database open, using its device/inode rather than just
its filename. It works with an already running collector. Access restrictions
can hide processes, so “not detected” does not prove no collector exists.
Freshness is reported separately: at most 60 seconds is fresh, older data is stale,
and a future timestamp is flagged as a clock mismatch. A detected process can
still have stale data; fresh data can remain after a collector stops. A new
collector normally needs about 30 seconds before its first batch is saved.

This is an informational command: exit 0 means the report completed, even for
missing or stale history; exit 1 indicates an error reading status and exit 2
indicates invalid arguments. `LANTERN_DB` and the usual default path rules apply.

## Collection and storage

`--collect` stays in the foreground and sleeps between five-second samples.
The first sample arrives after five seconds. A single SQLite connection commits
six samples together, approximately every 30 seconds. Ctrl+C and SIGTERM stop
collection and save any pending samples. For a persistent background collector,
run this command under your preferred service manager as your normal user.
Run one collector per database.

The database location, in precedence order:

1. `LANTERN_DB`, an explicit file path (relative paths use the working directory).
2. `$XDG_STATE_HOME/lantern/lantern.db`, when XDG_STATE_HOME is absolute.
3. `$HOME/.local/state/lantern/lantern.db`.

The collector creates parent directories. Use the same override for collection
and graphs, for example:

```sh
export LANTERN_DB="$HOME/lantern-data/history.db"
./target/release/lantern --collect
# In another terminal, set the same LANTERN_DB before running --graph.
```

Raw history is retained for approximately seven days. Cleanup runs on collector
startup and once per day; between cleanups, some samples may be nearly eight days
old. SQLite reuses freed pages rather than shrinking the file after every cleanup.
Timestamps are stored as UTC Unix seconds and displayed in local time.

Unexpected termination can lose the latest uncommitted samples. Temporary write
failures are reported and retried each interval; the buffer stays limited to the
six newest samples, dropping older pending samples with a warning if necessary.
A failed final save returns a nonzero exit status. Cleanup failures retry after
one hour. Missing or unreadable metrics become `N/A`/NULL, never invented zeroes.
An unavailable log destination does not stop collection; diagnostic messages
cannot be delivered while that destination remains unavailable.

## Metrics and graphs

| Metric | Source and interpretation |
| --- | --- |
| CPU usage and load | Aggregate `/proc/stat` counter deltas; iowait counts as idle. Load averages come from `/proc/loadavg`. |
| Memory and swap | `/proc/meminfo`; memory used is total minus `MemAvailable`. |
| Network RX/TX | `/proc/net/dev`, summed over non-loopback interfaces. Virtual interfaces can count traffic at multiple layers. |
| Disk read/write | `/proc/diskstats` and `/sys/block`; whole backing devices, excluding partitions, common pseudo-devices, and stacked-device duplicates. |
| CPU temperature | Recognised CPU hwmon or thermal sensors when readable. |
| NVIDIA GPU | Utilisation, VRAM, and temperature from NVML for the first accessible GPU. |
| Hostname and uptime | `/proc/sys/kernel/hostname` and `/proc/uptime` in the current snapshot. |

On Orange Pi 4 Pro (Allwinner A733), the vendor thermal types
`cpul_thermal_zone` and `cpub_thermal_zone` identify the CPU clusters. Lantern uses
the hottest valid CPU cluster reading, independently of the thermal zone numbers.
GPU, NPU, and DDR zones are excluded. These names are documented in the
[Orange Pi 4 Pro manual](https://orangepi.net/wp-content/uploads/2026/01/OrangePi_4_Pro_A733_User-Manual_v1.4.pdf).
A missing cluster does not hide the other cluster's valid reading. The collector
reads the existing sysfs files; no sensor tool or extra process is required.
After updating, temperature appears in new samples; old missing readings remain
unavailable. If a kernel uses different sensor names, inspect
`/sys/class/thermal/thermal_zone*/type` and the corresponding `temp` files.

Byte sizes use GiB (1024³ bytes); transfer rates use decimal B/s, kB/s, MB/s, etc.
Counter decreases and device/interface changes invalidate that interval and
establish a new baseline, avoiding artificial spikes.

Graphs show CPU usage %, CPU temperature °C, memory %, swap %, network RX/TX,
disk read/write, GPU utilisation, GPU temperature, and VRAM when recorded. Temperature uses its own scale, which can exceed
100 °C. Its curve shows the highest reading in each bucket so thermal peaks
remain visible; the summary still shows the average and exact raw peak/time.
The first page ends at the latest saved measurement and covers up to 24 hours
before it, even if collection stopped earlier. Short histories expand to use the
plot width instead of being squeezed into the end of an empty 24-hour chart.

In a terminal, history uses a fixed screen with a compact metric overview and one
larger selected graph. It fits the terminal and redraws in place without adding
scrollback. Controls use one pair of keys per task:

- **Left/Right arrows:** previous/next event and its matching graph.
- **Up/Down arrows:** CPU, RAM/swap, network RX, network TX, disk read, disk write, GPU use/temperature, or VRAM.
- **L/R:** preceding/following 24-hour period. Press R until the latest page to
  follow new measurements again.
- **Q:** quit and restore the previous terminal screen. Ctrl+C also exits safely.

The CPU view overlays a cyan solid usage curve (mean per bucket, 0–100% on the
left axis) and a yellow dotted temperature curve (peak per bucket, °C on the right
axis). Magenta × marks overlap; the two scales are independent. Missing readings
remain N/A. Metric curves are coloured, and text/shapes also convey
the meaning without relying on colour alone. The terminal honours `NO_COLOR`;
unset it to enable colours when that environment variable is present.

The GPU view uses the same two-axis layout: blue utilisation at 0–100% and yellow
temperature in °C. Its latest VRAM used/total GiB appears below, with a separate
VRAM percentage graph available through Up/Down. Temperature or VRAM data remains
visible even when utilisation is unavailable. These views use already saved NVIDIA
measurements; they add no collector reads. This does not add support for the
Orange Pi's integrated GPU.

RAM/swap overlays two curves: RAM is solid and swap is dotted, with colours
and labels identifying both.
RAM and swap share a 0–100% scale, each relative to its own capacity. The detail
line gives used/total GiB at the latest sample on the page; it distinguishes
missing readings from disabled swap. Network RX, network TX, disk read, and disk write each have a separate graph
and an independent rate scale. Each retains its average, raw peak and peak time.

At 24 terminal rows or more, RX/TX views also show separate group averages for
**Ethernet**, **Wi-Fi**, **Virtual**, and **Unknown**, including peak receive/transmit
packet errors and drops per second for the displayed direction. Error/drop peaks
keep brief incidents visible even across a whole day. Only groups with
measurements appear. Use at least 80 columns to see all detail fields. Traffic uses page averages; error/drop rates use page maxima, not lifetime counters. RX and TX
remain separate graphs. The main curve and network events still use all non-loopback
interfaces, so virtual interfaces can count the same traffic at several layers.
The physical group rates avoid adding Docker/bridge/veth traffic to hardware rates.
Virtual totals themselves can still count multiple layers; they are not internet
usage or per-container attribution.

Classification uses `/sys/class/net` device metadata, not interface names. Missing
metadata stays Unknown. Group rates and error/drop deltas use the existing
`/proc/net/dev` reading; only small sysfs metadata reads are added per sample.
Changing the members of one group resets that group's baseline without discarding
other groups. Individual decreasing counters become N/A for that interval.
Storage is bounded to four groups and six optional rates per group per sample,
regardless of container count. Sampling remains every five seconds, committing in
batches of six. A 32-second before/after collector check on the development PC
measured about 0.34% of one CPU core in both builds and RSS of 24,332 versus 24,524
KiB (+192 KiB). This short check is not a Hermit/Orange Pi benchmark. Old history has N/A group details; restart the updated collector to
begin saving them. No packet capture, new background task, or external command is used.

Network/disk detail lines estimate transferred bytes over observed intervals,
marked `~`, with the included seconds shown beside each direction. These are
approximations derived from saved rates and whole-second timestamps, not exact
interface/disk counters or totals across missing history. Only adjacent intervals
of 1–6 seconds contribute; longer gaps, duplicate/reversed timestamps and missing
or invalid rates are excluded. A sample immediately before the page can establish
its first interval, clipped to the page boundary. No consecutive readings means
N/A, whereas measured zero traffic can produce a zero estimate. These summaries
are computed while reading history and add no work to the collector.

Selecting an event opens its metric automatically, including swap and disk events.
CPU/temperature events open CPU; memory/swap events open RAM/swap; RX/TX events
open the corresponding RX or TX graph; read/write events open the corresponding
disk graph. A white vertical
marker and ◆ mark the event's exact raw peak and timestamp on that graph, even
when the mean curve smooths the peak. Events whose peak falls in the five-minute
context before a page are explicitly marked “outside page” rather than placed at
a false position. The selected event is preserved across refreshes while it remains
in the result set. Up/Down allows independent metric browsing; Left/Right reconnects
the graph to an event. No extra collector readings or database columns are needed.

The latest view checks for saved measurements every five seconds and redraws when
new history arrives. Browsing older periods keeps their time range stationary.
Navigation stops at the oldest and newest stored measurements, and empty periods
remain browsable. At least 32 columns and 12 rows are needed; on small screens the
overview table is hidden to leave room for the graph and controls.
When output or input is redirected, the latest page prints separate plain-text
charts once without waiting for keys or adding colours/terminal control sequences.

Usage and rate curves connect bucket means; the temperature curve connects bucket peaks; brief gaps between normal five-second readings are
connected across short time spans, while missing buckets from longer pauses stay blank.
CPU use and RAM/swap stay at 0–100%; GPU use and VRAM also stay at 0–100% in the interactive view. Summary averages
weight individual valid samples equally; peaks and their timestamps come from raw
samples, even when a short spike is smoothed in the curve. With the normal
five-second cadence, sample averages approximate time averages.

Full-screen mode follows the actual terminal dimensions, including resizing.
Plain-text output uses `COLUMNS`, then detected width, with an 80-column fallback
and a 160-column plot cap. `COLUMNS=40 lantern --graph | cat` produces narrow text.
Graphs read history without creating a database and explain how to start collection
when no history exists. Recent buffered samples appear after commit.

## Events generated when viewing history

`lantern` now opens the history view by default (`--graph` remains an alias).
Events are computed from raw saved measurements when a page loads and when new
committed data refreshes the view. The collector never runs event analysis.
Events stay in viewer memory: no event table, background analyzer, or notification
service is created. Closing and reopening recomputes the same results from the
same history. Switching metrics does not repeat the analysis.

Events appear above the graph, newest first. Use **Left/Right arrows** to browse their
time, observed duration, raw peak, rule, and process evidence. At least 16 terminal
rows are needed for event details; 24 show the rule as well. Smaller screens show
the count. Results apply to the selected history page; L/R changes both graphs
and events. At most the latest 200 events are retained in memory per page, with
the total count still shown. Redirected output prints these events once.

Initial rules are deliberately explicit, not a learned diagnosis:

| Reading | Event condition |
| --- | --- |
| CPU / memory | At least 90% |
| Swap occupancy | At least 50%; this is occupancy, not evidence of active swapping |
| CPU temperature | At least 80 °C; a review threshold, not a hardware safety limit |
| GPU utilisation | At least 95% |
| Network RX / TX | At least 6× the prior mean, with a 1 MB/s minimum |
| Disk read / write | At least 6× the prior mean, with a 10 MB/s minimum |

Every rule requires at least 30 seconds and seven distinct readings. Contiguous
high readings form one event. Missing values and gaps over ten seconds break an
episode; duplicate timestamps cannot establish persistence. Rate rules need at
least twelve preceding readings spanning at least 55 seconds, using up to five
minutes of history. Their threshold stays fixed during an episode. The reader
includes up to five minutes before a page for context; durations describe the
observed part of an episode, not necessarily its true beginning. Normal backups
or Immich jobs may satisfy these rules. Brief spikes can still be seen in graph
peak statistics but do not generate an event.

To offer historical process evidence, the collector reads `/proc/PID/stat` once
per visible process each five-second interval. It keeps only consecutive CPU
counters in memory and saves at most two process records per sample: the largest
observed CPU user and largest resident-memory user. No command arguments,
environment variables, process lists, or stack traces are stored. RSS is an
approximate resident size including shared pages. Field definitions follow the
[Linux kernel proc documentation](https://docs.kernel.org/filesystems/proc.html).
CPU process percentages are shares of the whole machine, matching the CPU graph.
PID and process start time identify consecutive readings, avoiding PID reuse.

CPU/memory events show the largest observed user at the event's raw peak as a
**possible contributor**, not a proven cause. Short-lived or inaccessible
processes can be missed. Container processes may have generic names; services
and containers are not resolved. Other event types show attribution unavailable.
Old samples show unknown process evidence; viewing present processes cannot
reconstruct a historical cause.

Restart the collector with the new binary to begin saving the new network details.
It transactionally upgrades schema versions 1 or 2 to 3 and preserves existing samples
and process evidence. Viewing an old database works read-only without migrating it.
Older Lantern binaries cannot open schema version 3; use the updated viewer and collector together.
Process evidence expires with its sample under the same retention policy.
Automatic startup is optional: `scripts/install-user.sh` configures the user service.
Building or running Lantern alone never installs a service.

## Optional GPU support

The default build dynamically loads NVIDIA's `libnvidia-ml.so.1`; it works without
that library or a GPU. Unavailable readings are omitted or shown as `N/A`. NVML
is initialised once per collector process; restart after installing a driver or
making a previously unavailable GPU accessible. No `nvidia-smi` process is spawned.

To compile without NVML support:

```sh
cargo build --release --locked --no-default-features
```

## Development and scope

For rust-analyzer, install the standard library sources with
`rustup component add rust-src`. The workspace's VS Code settings make a normal
`~/.cargo/bin` Rust installation available to rust-analyzer and new integrated
terminals. After installing Rust or changing these settings, run
**rust-analyzer: Restart server** from the Command Palette and open a new terminal.
For an external shell, load Rust with `. "$HOME/.cargo/env"` if Cargo is not on PATH.

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --no-default-features
```

Tests use parser fixtures and temporary databases. CLI tests check output and
errors without asserting live hardware values. Signal/pipe tests need local Unix
sockets, which some restricted sandboxes disallow.

`metrics` collects data, `collector` schedules and buffers it, `storage` persists
it, and `cli`/`graph` present it. The library exposes the shared `SystemSample`,
SQLite range queries (including streaming reads), and history bucketing so future
interfaces can reuse them.

Lantern 1.0.1 provides local collection and interactive history. It has no remote
API, notification service, or multi-GPU view. The optional installer configures
automatic collection; building the binary alone does not install a service.
