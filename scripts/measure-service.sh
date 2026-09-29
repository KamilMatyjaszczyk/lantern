#!/usr/bin/env bash
# Read systemd accounting; no extra collector or database is started.
set -euo pipefail
seconds=${1:-60}
if (($# > 1)) || [[ ! $seconds =~ ^[0-9]+$ ]] || ((${#seconds} > 4)) || ((10#$seconds < 10 || 10#$seconds > 3600)); then
    printf 'Usage: scripts/measure-service.sh [10..3600 seconds, default 60]\n' >&2
    exit 2
fi
seconds=$((10#$seconds))
read_stats() {
    local key value
    pid= memory= restarts=
    while IFS='=' read -r key value; do
        case $key in
            MainPID) pid=$value ;;
            MemoryCurrent) memory=$value ;;
            NRestarts) restarts=$value ;;
        esac
    done < <(systemctl --user show lantern.service -p MainPID -p MemoryCurrent -p NRestarts)
    [[ ${memory:-} != 18446744073709551615 && ${pid:-0} =~ ^[1-9][0-9]*$ && ${memory:-} =~ ^[0-9]+$ && ${restarts:-} =~ ^[0-9]+$ ]] || {
        printf 'Lantern service is not running or accounting is unavailable.\n' >&2; exit 1;
    }
    # utime + stime from /proc, without relying on optional systemd CPU accounting.
    # Strip everything through the last ')' first, so spaces in comm are harmless.
    cpu=$(awk '{sub(/^.*\) /, ""); printf "%.0f\n", $12 + $13}' "/proc/$pid/stat")
    [[ $cpu =~ ^[0-9]+$ ]] || { printf 'Cannot read process CPU time.\n' >&2; exit 1; }
}
ticks=$(getconf CLK_TCK)
[[ $ticks =~ ^[1-9][0-9]*$ ]] || exit 1
read_stats
first_pid=$pid; first_cpu=$cpu; first_restarts=$restarts
read -r first_time _ < /proc/uptime
printf 'Measuring existing Lantern service (PID %s) for %s seconds…\n' "$pid" "$seconds"
sleep "$seconds"
read_stats
read -r last_time _ < /proc/uptime
if [[ $pid != "$first_pid" || $restarts != "$first_restarts" ]] || ((cpu < first_cpu)); then
    printf 'Service restarted during measurement; rerun for a stable interval.\n' >&2
    exit 1
fi
rss=$(awk '/^VmRSS:/ {print $2}' "/proc/$pid/status")
[[ $rss =~ ^[0-9]+$ ]] || { printf 'Cannot read process RSS.\n' >&2; exit 1; }
awk -v elapsed="$(awk -v end="$last_time" -v start="$first_time" 'BEGIN {print end-start}')" \
    -v ticks="$ticks" -v before="$first_cpu" -v after="$cpu" -v rss="$rss" -v memory="$memory" 'BEGIN {
    if (elapsed <= 0) exit 1;
    printf "CPU: %.3f %% of one core (%.1f seconds)\n", (after-before)/ticks/elapsed*100, elapsed;
    printf "Process RSS: %.2f MiB\nService memory, including charged cache: %.2f MiB\n", rss/1024, memory/1048576;
}'
