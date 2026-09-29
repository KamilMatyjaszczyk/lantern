use std::{
    error::Error,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    time::{Duration, Instant, SystemTime},
};

use lantern::{
    model::{SystemSample, unix_timestamp},
    storage::{self, Database, default_db_path},
};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    low_level::{pipe, unregister},
};

use crate::metrics::Sampler;

const INTERVAL: Duration = Duration::from_secs(5);
const BATCH_SIZE: usize = 6;
const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub fn run() -> Result<(), Box<dyn Error>> {
    let mut shutdown = Shutdown::new()?;
    let path = default_db_path()?;
    let mut database = Database::open(&path)?;
    let mut retention = Retention {
        next_cleanup: Instant::now(),
    };
    retention.run_if_due(&database, Instant::now(), SystemTime::now());
    let mut sampler = Sampler::new();
    let mut processes = crate::metrics::process::ProcessSampler::new();
    let mut pending = PendingSamples::new();
    // A closed log pipe or full log destination must not abort collection.
    let _ = writeln!(
        io::stderr().lock(),
        "Lantern collector: every 5 seconds, saves about every 30 seconds → {}\nPress Ctrl+C to stop (saves pending samples).",
        path.display()
    );

    // The first five seconds establish rates over a complete interval.
    let mut deadline = Instant::now() + INTERVAL;
    while !shutdown.wait_until(deadline)? {
        match sampler.sample() {
            Ok(mut sample) => {
                processes.sample(&mut sample);
                if pending.push(sample) {
                    let _ = writeln!(
                        io::stderr().lock(),
                        "lantern: history buffer full after a write failure; oldest pending sample dropped"
                    );
                }
                if pending.is_full()
                    && let Err(error) = pending.flush(&mut database)
                {
                    let _ = writeln!(
                        io::stderr().lock(),
                        "lantern: pending samples were not saved; will retry next interval: {error}"
                    );
                }
            }
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "lantern: sample was not collected: {error}"
                );
            }
        }
        retention.run_if_due(&database, Instant::now(), SystemTime::now());
        deadline = next_deadline(deadline, Instant::now());
    }
    if let Err(error) = pending.flush(&mut database) {
        let _ = writeln!(
            io::stderr().lock(),
            "lantern: could not save {} pending samples during shutdown",
            pending.samples.len()
        );
        return Err(error.into());
    }
    let _ = writeln!(io::stderr().lock(), "Lantern collector stopped.");
    Ok(())
}

struct Retention {
    next_cleanup: Instant,
}

impl Retention {
    fn run_if_due(&mut self, database: &Database, now: Instant, wall_time: SystemTime) {
        if now < self.next_cleanup {
            return;
        }
        let result = unix_timestamp(wall_time)
            .map_err(storage::Error::from)
            .and_then(|timestamp| {
                database.delete_before(timestamp.saturating_sub(RETENTION_SECONDS))
            });
        let delay = match result {
            Ok(_) => CLEANUP_INTERVAL,
            Err(error) => {
                let _ = writeln!(
                    io::stderr().lock(),
                    "lantern: history cleanup failed; retrying in one hour: {error}"
                );
                Duration::from_secs(60 * 60)
            }
        };
        self.next_cleanup = now + delay;
    }
}

/// Normally six samples (30 seconds). Failed writes retain the batch for a
/// later attempt; prolonged failures retain only the six most recent samples.
struct PendingSamples {
    samples: Vec<SystemSample>,
}

impl PendingSamples {
    fn new() -> Self {
        Self {
            samples: Vec::with_capacity(BATCH_SIZE),
        }
    }

    /// Returns true if a sample had to be dropped to keep memory bounded.
    fn push(&mut self, sample: SystemSample) -> bool {
        let dropped = self.is_full();
        if dropped {
            self.samples.remove(0);
        }
        self.samples.push(sample);
        dropped
    }

    fn is_full(&self) -> bool {
        self.samples.len() == BATCH_SIZE
    }

    fn flush(&mut self, database: &mut Database) -> Result<(), storage::Error> {
        database.insert_batch(&self.samples)?;
        // Clear only after commit succeeds; retain the allocation for reuse.
        self.samples.clear();
        Ok(())
    }
}

fn next_deadline(previous: Instant, now: Instant) -> Instant {
    let next = previous + INTERVAL;
    if next > now { next } else { now + INTERVAL }
}

/// Signals write into a local socket pair. A timed blocking read lets the same
/// thread sleep until either the next sample or shutdown, without polling.
struct Shutdown {
    reader: UnixStream,
    handlers: Vec<SigId>,
}

impl Shutdown {
    fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        let mut shutdown = Self {
            reader,
            handlers: Vec::with_capacity(2),
        };
        for signal in [SIGINT, SIGTERM] {
            shutdown
                .handlers
                .push(pipe::register(signal, writer.try_clone()?)?);
        }
        Ok(shutdown)
    }

    fn wait_until(&mut self, deadline: Instant) -> io::Result<bool> {
        wait_for_shutdown(&mut self.reader, deadline)
    }
}

impl Drop for Shutdown {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            unregister(handler);
        }
    }
}

fn wait_for_shutdown(reader: &mut UnixStream, deadline: Instant) -> io::Result<bool> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        reader.set_read_timeout(Some(remaining))?;
        match reader.read(&mut [0_u8; 1]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "shutdown channel closed",
                ));
            }
            Ok(_) => return Ok(true),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lantern-buffer-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn database(&self) -> Database {
            Database::open(self.0.join("lantern.db")).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn buffer_stays_bounded_and_retains_newest_samples_after_prolonged_failure() {
        let mut pending = PendingSamples::new();
        for timestamp in 0..36 {
            assert_eq!(pending.push(SystemSample::new(timestamp)), timestamp >= 6);
            assert!(pending.samples.len() <= 6);
        }
        assert_eq!(
            pending
                .samples
                .iter()
                .map(|sample| sample.timestamp)
                .collect::<Vec<_>>(),
            vec![30, 31, 32, 33, 34, 35]
        );
    }

    #[test]
    fn retention_runs_at_startup_and_then_once_per_day() {
        let fixture = Fixture::new();
        let database = fixture.database();
        let now = Instant::now();
        let wall = std::time::UNIX_EPOCH + Duration::from_secs(RETENTION_SECONDS as u64 + 100);
        let mut retention = Retention { next_cleanup: now };
        database.insert(&SystemSample::new(99)).unwrap();
        database.insert(&SystemSample::new(100)).unwrap();
        retention.run_if_due(&database, now, wall);
        assert_eq!(
            database.query_range(0, 200).unwrap(),
            vec![SystemSample::new(100)]
        );
        database.insert(&SystemSample::new(99)).unwrap();
        retention.run_if_due(&database, now + INTERVAL, wall);
        assert_eq!(database.query_range(0, 200).unwrap().len(), 2);
        retention.run_if_due(&database, now + CLEANUP_INTERVAL, wall + CLEANUP_INTERVAL);
        assert!(database.query_range(0, 200).unwrap().is_empty());
    }

    #[test]
    fn partial_batch_flushes_and_repeated_empty_flush_does_not_duplicate_data() {
        let fixture = Fixture::new();
        let mut database = fixture.database();
        let mut pending = PendingSamples::new();
        pending.push(SystemSample::new(5));
        pending.push(SystemSample::new(10));
        assert!(database.query_range(0, 20).unwrap().is_empty());
        pending.flush(&mut database).unwrap();
        assert!(pending.samples.is_empty());
        pending.flush(&mut database).unwrap();
        assert_eq!(
            database.query_range(0, 20).unwrap(),
            vec![SystemSample::new(5), SystemSample::new(10)]
        );
    }

    #[test]
    fn failed_flush_keeps_samples_until_a_successful_commit() {
        let fixture = Fixture::new();
        let mut database = fixture.database();
        let mut pending = PendingSamples::new();
        for timestamp in 0..6 {
            pending.push(SystemSample::new(timestamp));
        }
        pending.samples[2].cpu_percent = Some(101.0);
        assert!(pending.flush(&mut database).is_err());
        assert!(pending.is_full());
        assert!(database.query_range(0, 10).unwrap().is_empty());
        pending.samples[2].cpu_percent = Some(50.0);
        let expected = pending.samples.clone();
        pending.flush(&mut database).unwrap();
        assert!(pending.samples.is_empty());
        assert_eq!(database.query_range(0, 10).unwrap(), expected);
    }

    #[test]
    fn schedule_does_not_accumulate_normal_collection_time() {
        let first = Instant::now();
        assert_eq!(
            next_deadline(first, first + Duration::from_millis(50)),
            first + INTERVAL
        );
    }

    #[test]
    fn missed_intervals_sleep_instead_of_catching_up_in_a_busy_loop() {
        let first = Instant::now();
        let delayed = first + Duration::from_secs(16);
        assert_eq!(next_deadline(first, delayed), delayed + INTERVAL);
        assert_eq!(
            next_deadline(first, first + INTERVAL),
            first + INTERVAL + INTERVAL
        );
    }

    #[test]
    fn queued_shutdown_wakes_without_waiting_for_the_deadline() {
        let (mut reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(&[1]).unwrap();
        assert!(wait_for_shutdown(&mut reader, Instant::now() + INTERVAL).unwrap());
    }

    #[test]
    fn deadline_and_closed_channel_are_distinct_from_shutdown() {
        let (mut reader, writer) = UnixStream::pair().unwrap();
        assert!(
            !wait_for_shutdown(&mut reader, Instant::now() + Duration::from_millis(5)).unwrap()
        );
        drop(writer);
        assert!(wait_for_shutdown(&mut reader, Instant::now() + INTERVAL).is_err());
    }
}
