# Lantern 1.0.1

Lantern is a lightweight Linux system monitor written in Rust. It collects resource
usage in the background and displays saved history, graphs, and events in your
terminal. It is designed for small servers, homelabs, and single-board computers,
including Orange Pi systems running Armbian.

The collector samples every five seconds and saves batches approximately every
30 seconds. Event analysis runs when you open the history view. Data stays in a
local SQLite database, with approximately seven days of history retained.

## Features

- CPU usage and temperature together, plus RAM and swap history.
- Separate network RX/TX and disk read/write graphs.
- Ethernet, Wi-Fi, and virtual network summaries with packet errors and drops.
- NVIDIA GPU usage, temperature, and VRAM when supported and enabled.
- Events linked to graph peaks, with possible process contributors for CPU and RAM.
- A standalone status command, automatic startup, and a service resource-use check.

Lantern runs as your normal user. SQLite is bundled; no external database server,
Python runtime, web server, or container is required.

## Requirements

To build and install on the target machine:

- Linux with readable `/proc` and `/sys`.
- Current stable Rust/Cargo and a C compiler/linker for bundled SQLite.
- Bash, standard core utilities, `tar`, and `gzip`.
- A working systemd user session for automatic collection.
- `sudo` access for enabling startup without an active login, if needed.
- Internet access for the initial Rust and dependency downloads.

The installer builds for the machine it runs on. An x86 PC binary cannot run on
an ARM board. Builds use one job at a time by default to limit resource usage.
Rust and the C compiler are needed for building and updating, not for running the
installed collector.

On Armbian, Debian, or Ubuntu, install the build prerequisites:

```sh
sudo apt update
sudo apt install --no-install-recommends build-essential curl ca-certificates
```

If Rust is not installed, install the minimal stable toolchain as your normal user:

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /tmp/lantern-rustup.sh
sh /tmp/lantern-rustup.sh -y --profile minimal --default-toolchain stable
. "$HOME/.cargo/env"
```

See the [official Rust installation guide](https://rust-lang.org/tools/install/).
For an existing rustup installation, use `rustup update stable`.

Check that your systemd user session is available:

```sh
systemctl --user show-environment
```

On stripped-down Armbian installations, user-session support may need restoring:

```sh
sudo apt install --no-install-recommends libpam-systemd dbus-user-session
```

Log out completely and log back in after installing those packages. Run the
installer from a normal user login, rather than a root shell.

## Install

Download or clone this repository onto the target machine. Open the project
directory containing `Cargo.toml`, then run:

```sh
./scripts/install-user.sh
~/.local/bin/lantern --status
~/.local/bin/lantern
```

Run the installer **without `sudo` in front of it**. Stop any manually running
`--collect` process with Ctrl+C first. The installer checks for visible collectors
using the same database before starting its service.

The installer:

1. Builds a native release without NVIDIA support.
2. Installs the launcher in `~/.local/bin` and the binary in `~/.local/libexec/lantern`.
3. Saves the database path for both the collector and the viewer.
4. Installs and enables `lantern.service` as a systemd user service.
5. Enables login lingering when needed so collection starts at boot and continues
   after you log out. Only this step uses `sudo`.

Lingering applies to the user's systemd manager, including other enabled user
services. The collector itself stays unprivileged. See
[systemd's lingering documentation](https://www.freedesktop.org/software/systemd/man/latest/loginctl.html).

The first saved batch normally appears after about 30 seconds. To use the short
`lantern` command, add this line to your shell configuration if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Transfer a source archive

To prepare the source package on another PC, run this from the project directory:

```sh
./scripts/package-hermit.sh
```

This creates `lantern-hermit.tar.gz` in the project directory. It includes source
code, installation scripts, and documentation; build output and databases are
excluded. Copy the archive to the target machine, then run there:

```sh
tar -xzf lantern-hermit.tar.gz
cd lantern
./scripts/install-user.sh
```

### Install a prebuilt binary

A compatible executable built for the target machine can be installed with:

```sh
./scripts/install-user.sh --binary /path/to/lantern
```

Add `--no-start` to install files without changing service or boot settings.
A later installation without that option activates the service.

## Usage

```sh
lantern              # Open saved graphs and events
lantern --graph      # Explicit history view
lantern --snapshot   # Print a current reading without using the database
lantern --status     # Check the collector and saved-history freshness
lantern --collect    # Run collection manually in the foreground
lantern --version
lantern --help
```

Use one collector per database. If the service is running, open `lantern` directly;
you do not need another `--collect` process. Closing the viewer leaves the service
running.

| Key | Action |
| --- | --- |
| Up / Down | Switch metrics |
| Left / Right | Select an event and its graph |
| L / R | Move to older / newer 24-hour history pages |
| Q | Close the viewer |

History opens at the latest saved measurement and shows up to the preceding
24 hours. The metric overview appears at 32 terminal rows or more. An 80-column
terminal is recommended for complete network details. Colours respect `NO_COLOR`.

## Service and resource checks

```sh
systemctl --user status lantern.service
systemctl --user is-enabled lantern.service
loginctl show-user "$(id -un)" --property=Linger
journalctl --user -u lantern.service -n 30 --no-pager
lantern --status
```

The service restarts ten seconds after a process failure, with a limit of ten
starts in five minutes. A manual stop stays stopped. Normal shutdown allows the
collector to flush pending samples. Automatic restart does not detect a live,
stalled process or fix a full disk.

To measure the existing service, run this from the project directory:

```sh
./scripts/measure-service.sh 60
```

It reports CPU usage as a percentage of one core, process RSS, and service memory
including charged cache. CPU covers the measurement interval; memory values are
read at the end. No additional collector starts. A service restart during the
measurement invalidates the result. The helper uses `awk` and `getconf`.

After a planned reboot, check that lingering is enabled, the service is active,
and `lantern --status` reports fresh data after around 30–60 seconds.

## Updates and history

Replace or update the project files, then run the same installer again:

```sh
./scripts/install-user.sh
```

The new binary is built and checked before the existing service stops. The
installer then stops collection gracefully, replaces the files, and starts the
service again. Saved history and the configured database path are preserved.
There is a short collection gap during replacement. Close and reopen existing
viewers to use the updated version.

At first installation, the database path is saved in:

```text
${XDG_CONFIG_HOME:-$HOME/.config}/lantern/environment.sh
```

The default database is `~/.local/state/lantern/lantern.db`. Set `LANTERN_DB` or an
absolute `XDG_STATE_HOME` before the first installation to select existing history.
The installed launcher uses the saved path unless you explicitly override
`LANTERN_DB`. Subsequent installations retain the saved choice.

Database migrations preserve existing samples. Older binaries may not support a
newer database schema, so replacing a binary does not automatically downgrade its
history format.

## Stop or disable collection

```sh
systemctl --user stop lantern.service
systemctl --user start lantern.service

# Stop collection and disable automatic startup:
systemctl --user disable --now lantern.service
```

Disabling collection preserves history. Lingering is left enabled because other
user services may depend on it.

## Manual builds and NVIDIA support

To build and run without installing a service:

```sh
cargo build --release --locked --no-default-features
./target/release/lantern --collect
```

Open `./target/release/lantern` in another terminal to view the saved data.

For optional NVIDIA support, build with the default features:

```sh
cargo build --release --locked
./scripts/install-user.sh --binary ./target/release/lantern
```

GPU readings use NVIDIA's NVML library when available. Integrated Orange Pi GPUs
are not currently supported.

## Technical documentation

See the [technical reference](docs/REFERENCE.md) for metric definitions, event
thresholds, storage behavior, and development checks.

`tests/install_user.py` checks installation and updates in an isolated home with
mocked systemd commands and a short-lived real collector. It requires Python and
Bubblewrap for development testing; neither is a Lantern runtime dependency.
The test project is mounted at `/tmp/work`, and the isolated home must contain
`.lantern-isolated-test`. Hardware startup and resource usage should be verified
on the machine where Lantern is deployed.
