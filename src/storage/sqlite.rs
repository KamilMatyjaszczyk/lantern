use std::{env, fs, io, path::Path, time::Duration};

use rusqlite::{Connection, OpenFlags, OptionalExtension, Row, Statement, params};

use super::{Error, default_db_path};
use crate::model::{ProcessUsage, SystemSample};

const INSERT: &str = "INSERT INTO samples (
    timestamp, cpu_percent, load_1, load_5, load_15,
    memory_used, memory_total, swap_used, swap_total,
    network_rx_rate, network_tx_rate, disk_read_rate, disk_write_rate,
    cpu_temperature, gpu_utilisation, gpu_memory_used, gpu_memory_total, gpu_temperature,
    top_cpu_pid, top_cpu_name, top_cpu_start, top_cpu_usage,
    top_memory_pid, top_memory_name, top_memory_start, top_memory_usage,
    net_ethernet_rx, net_ethernet_tx, net_ethernet_rx_errors, net_ethernet_tx_errors, net_ethernet_rx_drops, net_ethernet_tx_drops,
    net_wifi_rx, net_wifi_tx, net_wifi_rx_errors, net_wifi_tx_errors, net_wifi_rx_drops, net_wifi_tx_drops,
    net_virtual_rx, net_virtual_tx, net_virtual_rx_errors, net_virtual_tx_errors, net_virtual_rx_drops, net_virtual_tx_drops,
    net_unknown_rx, net_unknown_tx, net_unknown_rx_errors, net_unknown_tx_errors, net_unknown_rx_drops, net_unknown_tx_drops
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45, ?46, ?47, ?48, ?49, ?50)";

const QUERY_V1: &str = "SELECT
    timestamp, cpu_percent, load_1, load_5, load_15,
    memory_used, memory_total, swap_used, swap_total,
    network_rx_rate, network_tx_rate, disk_read_rate, disk_write_rate,
    cpu_temperature, gpu_utilisation, gpu_memory_used, gpu_memory_total, gpu_temperature,
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL,
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL
FROM samples WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY timestamp, id";
const QUERY_V2: &str = "SELECT
    timestamp, cpu_percent, load_1, load_5, load_15,
    memory_used, memory_total, swap_used, swap_total,
    network_rx_rate, network_tx_rate, disk_read_rate, disk_write_rate,
    cpu_temperature, gpu_utilisation, gpu_memory_used, gpu_memory_total, gpu_temperature,
    top_cpu_pid, top_cpu_name, top_cpu_start, top_cpu_usage,
    top_memory_pid, top_memory_name, top_memory_start, top_memory_usage,
    NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL
FROM samples WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY timestamp, id";

const QUERY: &str = "SELECT
    timestamp, cpu_percent, load_1, load_5, load_15,
    memory_used, memory_total, swap_used, swap_total,
    network_rx_rate, network_tx_rate, disk_read_rate, disk_write_rate,
    cpu_temperature, gpu_utilisation, gpu_memory_used, gpu_memory_total, gpu_temperature,
    top_cpu_pid, top_cpu_name, top_cpu_start, top_cpu_usage,
    top_memory_pid, top_memory_name, top_memory_start, top_memory_usage,
    net_ethernet_rx, net_ethernet_tx, net_ethernet_rx_errors, net_ethernet_tx_errors, net_ethernet_rx_drops, net_ethernet_tx_drops,
    net_wifi_rx, net_wifi_tx, net_wifi_rx_errors, net_wifi_tx_errors, net_wifi_rx_drops, net_wifi_tx_drops,
    net_virtual_rx, net_virtual_tx, net_virtual_rx_errors, net_virtual_tx_errors, net_virtual_rx_drops, net_virtual_tx_drops,
    net_unknown_rx, net_unknown_tx, net_unknown_rx_errors, net_unknown_tx_errors, net_unknown_rx_drops, net_unknown_tx_drops
FROM samples WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY timestamp, id";

pub struct Database {
    connection: Connection,
    version: i64,
}

impl Database {
    /// Read history without creating directories, a database, or a schema.
    /// Missing files and uninitialised, empty SQLite databases have no history.
    pub fn open_existing(path: impl AsRef<Path>) -> Result<Option<Self>, Error> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "database path is empty").into(),
            );
        }
        let path = env::current_dir()?.join(path);
        match fs::metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                let tables: i64 = connection.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type = 'table'",
                    [],
                    |row| row.get(0),
                )?;
                if tables == 0 {
                    return Ok(None);
                }
                return Err(Error::UnsupportedSchema(version));
            }
            1 => {
                connection.prepare(QUERY_V1)?;
            }
            2 => {
                connection.prepare(QUERY_V2)?;
            }
            3 => {
                connection.prepare(QUERY)?;
            }
            other => return Err(Error::UnsupportedSchema(other)),
        }
        Ok(Some(Self {
            connection,
            version,
        }))
    }

    pub fn open_default() -> Result<Self, Error> {
        Self::open(default_db_path()?)
    }

    /// Open a filesystem database, creating parent directories and schema as needed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "database path is empty").into(),
            );
        }
        // Resolve relative paths so SQLite's special :memory: filename cannot
        // accidentally turn a configured persistent database into an in-memory one.
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            env::current_dir()?.join(path)
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let transaction = connection.transaction()?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                transaction.execute_batch(include_str!("schema.sql"))?;
                transaction.execute_batch(include_str!("processes.sql"))?;
                transaction.execute_batch(include_str!("network.sql"))?;
            }
            1 => {
                transaction.execute_batch(include_str!("processes.sql"))?;
                transaction.execute_batch(include_str!("network.sql"))?;
            }
            2 => transaction.execute_batch(include_str!("network.sql"))?,
            3 => {
                transaction.prepare(QUERY)?;
            }
            other => return Err(Error::UnsupportedSchema(other)),
        }
        transaction.commit()?;
        Ok(Self {
            connection,
            version: 3,
        })
    }

    pub fn insert(&self, sample: &SystemSample) -> Result<(), Error> {
        let mut statement = self.connection.prepare(INSERT)?;
        insert_sample(&mut statement, sample)
    }

    /// Find the first stored sample in [start, end) using the timestamp index.
    pub fn first_timestamp_in_range(&self, start: i64, end: i64) -> Result<Option<i64>, Error> {
        if end < start {
            return Err(Error::InvalidTimeRange);
        }
        Ok(self.connection.query_row(
            "SELECT timestamp FROM samples WHERE timestamp >= ?1 AND timestamp < ?2 ORDER BY timestamp LIMIT 1",
            params![start, end],
            |row| row.get(0),
        ).optional()?)
    }

    /// Oldest and newest stored timestamps, found by seeking in the index.
    pub fn timestamp_bounds(&self) -> Result<Option<(i64, i64)>, Error> {
        let (first, last): (Option<i64>, Option<i64>) = self.connection.query_row(
            "SELECT (SELECT timestamp FROM samples ORDER BY timestamp LIMIT 1),
                    (SELECT timestamp FROM samples ORDER BY timestamp DESC LIMIT 1)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(first.zip(last))
    }

    /// Delete only samples strictly older than the UTC cutoff.
    pub fn delete_before(&self, cutoff: i64) -> Result<usize, Error> {
        Ok(self
            .connection
            .execute("DELETE FROM samples WHERE timestamp < ?1", [cutoff])?)
    }

    /// Commit all samples together, or roll back the entire batch on failure.
    /// An empty batch does not start a transaction or write to disk.
    pub fn insert_batch(&mut self, samples: &[SystemSample]) -> Result<(), Error> {
        if samples.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        {
            // Prepare once for the batch and reuse the statement for every row.
            let mut statement = transaction.prepare(INSERT)?;
            for sample in samples {
                insert_sample(&mut statement, sample)?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Samples in [start, end), using UTC Unix seconds, ordered by timestamp.
    /// Duplicate timestamps are retained in insertion order.
    pub fn query_range(&self, start: i64, end: i64) -> Result<Vec<SystemSample>, Error> {
        let mut samples = Vec::new();
        self.visit_range(start, end, |sample| samples.push(sample))?;
        Ok(samples)
    }

    /// Stream a range so callers can aggregate history with bounded memory.
    pub fn visit_range(
        &self,
        start: i64,
        end: i64,
        mut visit: impl FnMut(SystemSample),
    ) -> Result<(), Error> {
        if end < start {
            return Err(Error::InvalidTimeRange);
        }
        let mut statement = self.connection.prepare(match self.version {
            1 => QUERY_V1,
            2 => QUERY_V2,
            _ => QUERY,
        })?;
        let rows = statement.query_map(params![start, end], read_sample)?;
        for row in rows {
            visit(row?);
        }
        Ok(())
    }
}

fn insert_sample(statement: &mut Statement<'_>, sample: &SystemSample) -> Result<(), Error> {
    // SQLite converts NaN to NULL. Reject it explicitly to preserve the
    // distinction between invalid input and an unavailable measurement.
    for (name, value) in [
        ("cpu_percent", sample.cpu_percent),
        ("load_1", sample.load_1),
        ("load_5", sample.load_5),
        ("load_15", sample.load_15),
        ("network_rx_rate", sample.network_rx_rate),
        ("network_tx_rate", sample.network_tx_rate),
        ("disk_read_rate", sample.disk_read_rate),
        ("disk_write_rate", sample.disk_write_rate),
        ("cpu_temperature", sample.cpu_temperature),
    ] {
        if value.is_some_and(|value| !value.is_finite()) {
            return Err(Error::InvalidSample(name));
        }
    }
    for process in [&sample.top_cpu, &sample.top_memory].into_iter().flatten() {
        if !process.usage.is_finite()
            || process.usage < 0.0
            || process.pid == 0
            || process.name.len() > 128
            || process.name.chars().any(char::is_control)
        {
            return Err(Error::InvalidSample("process evidence"));
        }
    }
    if sample.top_cpu.as_ref().is_some_and(|p| p.usage > 100.0) {
        return Err(Error::InvalidSample("process CPU"));
    }
    if sample
        .network_groups
        .iter()
        .flatten()
        .flatten()
        .any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(Error::InvalidSample("network details"));
    }
    let cpu = sample.top_cpu.as_ref();
    let memory = sample.top_memory.as_ref();
    statement.execute(params![
        sample.timestamp,
        sample.cpu_percent,
        sample.load_1,
        sample.load_5,
        sample.load_15,
        sql_bytes(sample.memory_used)?,
        sql_bytes(sample.memory_total)?,
        sql_bytes(sample.swap_used)?,
        sql_bytes(sample.swap_total)?,
        sample.network_rx_rate,
        sample.network_tx_rate,
        sample.disk_read_rate,
        sample.disk_write_rate,
        sample.cpu_temperature,
        sample.gpu_utilisation,
        sql_bytes(sample.gpu_memory_used)?,
        sql_bytes(sample.gpu_memory_total)?,
        sample.gpu_temperature,
        cpu.map(|p| p.pid),
        cpu.map(|p| p.name.as_str()),
        sql_bytes(cpu.map(|p| p.start_ticks))?,
        cpu.map(|p| p.usage),
        memory.map(|p| p.pid),
        memory.map(|p| p.name.as_str()),
        sql_bytes(memory.map(|p| p.start_ticks))?,
        memory.map(|p| p.usage),
        sample.network_groups[0][0],
        sample.network_groups[0][1],
        sample.network_groups[0][2],
        sample.network_groups[0][3],
        sample.network_groups[0][4],
        sample.network_groups[0][5],
        sample.network_groups[1][0],
        sample.network_groups[1][1],
        sample.network_groups[1][2],
        sample.network_groups[1][3],
        sample.network_groups[1][4],
        sample.network_groups[1][5],
        sample.network_groups[2][0],
        sample.network_groups[2][1],
        sample.network_groups[2][2],
        sample.network_groups[2][3],
        sample.network_groups[2][4],
        sample.network_groups[2][5],
        sample.network_groups[3][0],
        sample.network_groups[3][1],
        sample.network_groups[3][2],
        sample.network_groups[3][3],
        sample.network_groups[3][4],
        sample.network_groups[3][5],
    ])?;
    Ok(())
}

// SQLite integers are signed. Never wrap or round byte counts when converting.
fn sql_bytes(value: Option<u64>) -> rusqlite::Result<Option<i64>> {
    value
        .map(i64::try_from)
        .transpose()
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn read_bytes(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
        })
        .transpose()
}

fn read_process(row: &Row<'_>, offset: usize) -> rusqlite::Result<Option<ProcessUsage>> {
    let Some(pid) = row.get::<_, Option<u32>>(offset)? else {
        return Ok(None);
    };
    Ok(Some(ProcessUsage {
        pid,
        name: row.get(offset + 1)?,
        start_ticks: read_bytes(row, offset + 2)?.ok_or(rusqlite::Error::InvalidQuery)?,
        usage: row.get(offset + 3)?,
    }))
}

fn read_sample(row: &Row<'_>) -> rusqlite::Result<SystemSample> {
    Ok(SystemSample {
        timestamp: row.get(0)?,
        top_cpu: read_process(row, 18)?,
        top_memory: read_process(row, 22)?,
        cpu_percent: row.get(1)?,
        load_1: row.get(2)?,
        load_5: row.get(3)?,
        load_15: row.get(4)?,
        memory_used: read_bytes(row, 5)?,
        memory_total: read_bytes(row, 6)?,
        swap_used: read_bytes(row, 7)?,
        swap_total: read_bytes(row, 8)?,
        network_groups: [
            [
                row.get(26)?,
                row.get(27)?,
                row.get(28)?,
                row.get(29)?,
                row.get(30)?,
                row.get(31)?,
            ],
            [
                row.get(32)?,
                row.get(33)?,
                row.get(34)?,
                row.get(35)?,
                row.get(36)?,
                row.get(37)?,
            ],
            [
                row.get(38)?,
                row.get(39)?,
                row.get(40)?,
                row.get(41)?,
                row.get(42)?,
                row.get(43)?,
            ],
            [
                row.get(44)?,
                row.get(45)?,
                row.get(46)?,
                row.get(47)?,
                row.get(48)?,
                row.get(49)?,
            ],
        ],
        network_rx_rate: row.get(9)?,
        network_tx_rate: row.get(10)?,
        disk_read_rate: row.get(11)?,
        disk_write_rate: row.get(12)?,
        cpu_temperature: row.get(13)?,
        gpu_utilisation: row.get(14)?,
        gpu_memory_used: read_bytes(row, 15)?,
        gpu_memory_total: read_bytes(row, 16)?,
        gpu_temperature: row.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = env::temp_dir().join(format!(
                "lantern-storage-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> PathBuf {
            self.0.join("state/lantern/lantern.db")
        }
        fn open(&self) -> Database {
            Database::open(self.path()).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn full_sample(timestamp: i64) -> SystemSample {
        SystemSample {
            timestamp,
            top_cpu: Some(ProcessUsage {
                pid: 42,
                name: "worker".into(),
                start_ticks: 123,
                usage: 10.0,
            }),
            top_memory: Some(ProcessUsage {
                pid: 43,
                name: "database".into(),
                start_ticks: 124,
                usage: 1_000_000.0,
            }),
            cpu_percent: Some(12.5),
            load_1: Some(0.42),
            load_5: Some(0.31),
            load_15: Some(0.28),
            memory_used: Some(3_000_000_001),
            memory_total: Some(8_000_000_003),
            swap_used: Some(1_000_000_005),
            swap_total: Some(2_000_000_007),
            network_groups: std::array::from_fn(|g| {
                std::array::from_fn(|m| Some((g * 10 + m) as f64 + 0.25))
            }),
            network_rx_rate: Some(1234.5),
            network_tx_rate: Some(6789.25),
            disk_read_rate: Some(1011.75),
            disk_write_rate: Some(1213.125),
            cpu_temperature: Some(48.5),
            gpu_utilisation: Some(3),
            gpu_memory_used: Some(4_000_000_009),
            gpu_memory_total: Some(16_000_000_011),
            gpu_temperature: Some(44),
        }
    }

    #[test]
    fn creates_directories_and_persists_every_metric_across_reopen() {
        let fixture = Fixture::new();
        let sample = full_sample(1_800_000_000);
        {
            let database = fixture.open();
            database.insert(&sample).unwrap();
        }
        assert!(fixture.path().is_file());
        let database = fixture.open();
        assert_eq!(
            database
                .query_range(sample.timestamp, sample.timestamp + 1)
                .unwrap(),
            vec![sample]
        );
    }

    #[test]
    fn legacy_history_is_read_only_then_migrates_without_losing_samples() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
        let old = Connection::open(fixture.path()).unwrap();
        old.execute_batch(include_str!("schema.sql")).unwrap();
        old.execute(
            "INSERT INTO samples(timestamp,cpu_percent) VALUES(10,95)",
            [],
        )
        .unwrap();
        drop(old);
        let before = fs::read(fixture.path()).unwrap();
        let reader = Database::open_existing(fixture.path()).unwrap().unwrap();
        let samples = reader.query_range(0, 20).unwrap();
        assert_eq!(samples[0].cpu_percent, Some(95.0));
        assert!(samples[0].top_cpu.is_none());
        assert_eq!(before, fs::read(fixture.path()).unwrap());
        drop(reader);
        let mut writer = fixture.open();
        assert_eq!(writer.query_range(0, 20).unwrap(), samples);
        writer.insert_batch(&[full_sample(20)]).unwrap();
        drop(writer);
        let reader = Database::open_existing(fixture.path()).unwrap().unwrap();
        assert_eq!(reader.query_range(20, 21).unwrap(), vec![full_sample(20)]);
        assert_eq!(reader.query_range(0, 20).unwrap(), samples);
    }

    #[test]
    fn version_two_upgrades_atomically_preserving_evidence_and_unknown_network_history() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
        let old = Connection::open(fixture.path()).unwrap();
        old.execute_batch(include_str!("schema.sql")).unwrap();
        old.execute_batch(include_str!("processes.sql")).unwrap();
        old.execute("INSERT INTO samples(timestamp,top_cpu_pid,top_cpu_name,top_cpu_start,top_cpu_usage) VALUES(10,42,'worker',123,95)", []).unwrap();
        drop(old);
        let before = fs::read(fixture.path()).unwrap();
        let reader = Database::open_existing(fixture.path()).unwrap().unwrap();
        let rows = reader.query_range(0, 20).unwrap();
        assert_eq!(rows[0].top_cpu.as_ref().unwrap().name, "worker");
        assert_eq!(rows[0].network_groups, [[None; 6]; 4]);
        assert_eq!(before, fs::read(fixture.path()).unwrap());
        drop(reader);
        let mut writer = fixture.open();
        assert_eq!(writer.query_range(0, 20).unwrap(), rows);
        writer.insert(&full_sample(20)).unwrap();
        let mut invalid = full_sample(30);
        invalid.network_groups[2][4] = Some(f64::NAN);
        assert!(writer.insert_batch(&[full_sample(25), invalid]).is_err());
        assert!(writer.query_range(25, 40).unwrap().is_empty());
        let mut partial = SystemSample::new(40);
        partial.network_groups[1][0] = Some(0.0);
        partial.network_groups[1][4] = Some(1.25);
        writer.insert(&partial).unwrap();
        assert_eq!(writer.query_range(40, 41).unwrap(), vec![partial]);
        assert_eq!(writer.delete_before(21).unwrap(), 2);
    }

    #[test]
    fn invalid_process_evidence_rolls_back_batch_and_retention_removes_evidence() {
        let fixture = Fixture::new();
        let mut database = fixture.open();
        let mut invalid = full_sample(20);
        invalid.top_cpu.as_mut().unwrap().usage = f64::NAN;
        assert!(database.insert_batch(&[full_sample(10), invalid]).is_err());
        assert!(database.query_range(0, 30).unwrap().is_empty());
        let mut invalid = full_sample(20);
        invalid.top_cpu.as_mut().unwrap().start_ticks = u64::MAX;
        assert!(database.insert(&invalid).is_err());
        database.insert(&full_sample(10)).unwrap();
        database.delete_before(11).unwrap();
        assert!(database.query_range(0, 30).unwrap().is_empty());
    }

    #[test]
    fn first_timestamp_respects_range_boundaries_and_empty_history() {
        let fixture = Fixture::new();
        let database = fixture.open();
        assert_eq!(database.first_timestamp_in_range(10, 20).unwrap(), None);
        assert_eq!(database.timestamp_bounds().unwrap(), None);
        for timestamp in [30, 9, 20, 10, 15] {
            database.insert(&SystemSample::new(timestamp)).unwrap();
        }
        assert_eq!(database.first_timestamp_in_range(10, 20).unwrap(), Some(10));
        assert_eq!(database.first_timestamp_in_range(11, 20).unwrap(), Some(15));
        assert_eq!(database.first_timestamp_in_range(16, 20).unwrap(), None);
        assert!(database.first_timestamp_in_range(20, 10).is_err());
        assert_eq!(database.timestamp_bounds().unwrap(), Some((9, 30)));
    }

    #[test]
    fn history_reads_never_create_or_modify_the_database() {
        let fixture = Fixture::new();
        assert!(Database::open_existing(fixture.path()).unwrap().is_none());
        assert!(!fixture.path().parent().unwrap().exists());
        let database = fixture.open();
        database.insert(&full_sample(100)).unwrap();
        drop(database);
        let before = fs::read(fixture.path()).unwrap();
        let reader = Database::open_existing(fixture.path()).unwrap().unwrap();
        let mut samples = Vec::new();
        reader
            .visit_range(100, 101, |sample| samples.push(sample))
            .unwrap();
        assert_eq!(samples, vec![full_sample(100)]);
        assert!(reader.insert(&full_sample(101)).is_err());
        assert_eq!(before, fs::read(fixture.path()).unwrap());
    }

    #[test]
    fn empty_files_are_empty_history_but_corrupt_files_are_errors() {
        let fixture = Fixture::new();
        let path = fixture.0.join("empty.db");
        fs::write(&path, []).unwrap();
        assert!(Database::open_existing(&path).unwrap().is_none());
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        fs::write(&path, b"not a SQLite database").unwrap();
        assert!(Database::open_existing(&path).is_err());
    }

    #[test]
    fn nulls_partial_metrics_and_zeroes_remain_distinct() {
        let fixture = Fixture::new();
        let database = fixture.open();
        let missing = SystemSample::new(10);
        let mut zeroes = SystemSample::new(20);
        zeroes.cpu_percent = Some(0.0);
        zeroes.network_rx_rate = Some(0.0);
        zeroes.swap_used = Some(0);
        zeroes.swap_total = Some(0);
        zeroes.gpu_utilisation = Some(0);
        let mut partial = SystemSample::new(30);
        partial.gpu_temperature = Some(44);
        partial.memory_total = Some(8192);
        for sample in [&missing, &zeroes, &partial] {
            database.insert(sample).unwrap();
        }
        assert_eq!(
            database.query_range(0, 40).unwrap(),
            vec![missing, zeroes, partial]
        );
        let null_count: i64 = database
            .connection
            .query_row(
                "SELECT count(*) FROM samples WHERE cpu_percent IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(null_count, 2);
    }

    #[test]
    fn time_ranges_are_ordered_half_open_and_preserve_duplicate_timestamps() {
        let fixture = Fixture::new();
        let database = fixture.open();
        let mut duplicate = SystemSample::new(10);
        duplicate.cpu_percent = Some(50.0);
        for sample in [
            SystemSample::new(20),
            SystemSample::new(-1),
            SystemSample::new(10),
            duplicate.clone(),
        ] {
            database.insert(&sample).unwrap();
        }
        assert_eq!(
            database.query_range(-1, 20).unwrap(),
            vec![SystemSample::new(-1), SystemSample::new(10), duplicate]
        );
        assert!(database.query_range(20, 20).unwrap().is_empty());
        assert!(matches!(
            database.query_range(20, 10),
            Err(Error::InvalidTimeRange)
        ));
    }

    #[test]
    fn empty_database_and_independent_reader_work() {
        let fixture = Fixture::new();
        let writer = fixture.open();
        let reader = fixture.open();
        assert!(reader.query_range(0, 100).unwrap().is_empty());
        writer.insert(&full_sample(50)).unwrap();
        assert_eq!(reader.query_range(0, 100).unwrap(), vec![full_sample(50)]);
    }

    #[test]
    fn batch_round_trip_and_empty_batch_preserve_history() {
        let fixture = Fixture::new();
        let mut database = fixture.open();
        let samples = vec![full_sample(10), SystemSample::new(15), full_sample(20)];
        database.insert_batch(&samples).unwrap();
        database.insert_batch(&[]).unwrap();
        drop(database);
        assert_eq!(fixture.open().query_range(0, 30).unwrap(), samples);
    }

    #[test]
    fn retention_deletes_only_rows_older_than_cutoff() {
        let fixture = Fixture::new();
        let mut database = fixture.open();
        database
            .insert_batch(&[full_sample(9), full_sample(10), full_sample(11)])
            .unwrap();
        assert_eq!(database.delete_before(10).unwrap(), 1);
        assert_eq!(
            database.query_range(0, 20).unwrap(),
            vec![full_sample(10), full_sample(11)]
        );
        assert_eq!(database.delete_before(10).unwrap(), 0);
    }

    #[test]
    fn invalid_row_rolls_back_entire_batch_without_affecting_existing_data() {
        let fixture = Fixture::new();
        let mut database = fixture.open();
        database.insert(&full_sample(5)).unwrap();
        let mut invalid = full_sample(15);
        invalid.cpu_percent = Some(101.0);
        assert!(
            database
                .insert_batch(&[full_sample(10), invalid, full_sample(20)])
                .is_err()
        );
        assert_eq!(database.query_range(0, 30).unwrap(), vec![full_sample(5)]);
        database
            .insert_batch(&[full_sample(10), full_sample(15)])
            .unwrap();
        assert_eq!(
            database.query_range(0, 30).unwrap(),
            vec![full_sample(5), full_sample(10), full_sample(15)]
        );
    }

    #[test]
    fn failed_commit_rolls_back_and_can_be_retried_without_duplicates() {
        let fixture = Fixture::new();
        let mut writer = fixture.open();
        writer.connection.busy_timeout(Duration::ZERO).unwrap();
        let reader = fixture.open();
        // A reader permits the writes to begin but blocks the commit in the
        // default rollback-journal mode, exercising failure after all inserts.
        reader
            .connection
            .execute_batch("BEGIN; SELECT count(*) FROM samples;")
            .unwrap();
        let samples = vec![full_sample(10), full_sample(15)];
        assert!(writer.insert_batch(&samples).is_err());
        reader.connection.execute_batch("ROLLBACK").unwrap();
        assert!(writer.query_range(0, 30).unwrap().is_empty());
        writer.insert_batch(&samples).unwrap();
        assert_eq!(reader.query_range(0, 30).unwrap(), samples);
    }

    #[test]
    fn rejects_non_finite_values_and_integer_overflow_without_inserting() {
        let fixture = Fixture::new();
        let database = fixture.open();
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut sample = full_sample(50);
            sample.network_rx_rate = Some(value);
            assert!(matches!(
                database.insert(&sample),
                Err(Error::InvalidSample("network_rx_rate"))
            ));
        }
        let mut sample = full_sample(50);
        sample.memory_total = Some(u64::MAX);
        assert!(database.insert(&sample).is_err());
        assert!(database.query_range(0, 100).unwrap().is_empty());
        sample.memory_total = Some(i64::MAX as u64);
        sample.memory_used = Some(i64::MAX as u64 - 1);
        database.insert(&sample).unwrap();
        assert_eq!(database.query_range(0, 100).unwrap(), vec![sample]);
    }

    #[test]
    fn schema_rejects_invalid_percentages_and_inconsistent_memory() {
        let fixture = Fixture::new();
        let database = fixture.open();
        let mut sample = full_sample(50);
        sample.cpu_percent = Some(101.0);
        assert!(database.insert(&sample).is_err());
        sample.cpu_percent = Some(50.0);
        sample.memory_total = Some(1);
        assert!(database.insert(&sample).is_err());
        assert!(database.query_range(0, 100).unwrap().is_empty());
    }

    #[test]
    fn unsupported_schema_is_rejected_without_modification() {
        let fixture = Fixture::new();
        {
            let database = fixture.open();
            database.insert(&full_sample(50)).unwrap();
            database
                .connection
                .pragma_update(None, "user_version", 99)
                .unwrap();
        }
        assert!(matches!(
            Database::open(fixture.path()),
            Err(Error::UnsupportedSchema(99))
        ));
        let connection = Connection::open(fixture.path()).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        let count: i64 = connection
            .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 99);
        assert_eq!(count, 1);
    }

    #[test]
    fn invalid_paths_and_non_database_files_return_errors() {
        let fixture = Fixture::new();
        assert!(Database::open("").is_err());
        assert!(Database::open(&fixture.0).is_err());
        let file = fixture.0.join("not-a-database");
        fs::write(&file, b"this is not a SQLite database").unwrap();
        assert!(Database::open(&file).is_err());
        assert!(Database::open(file.join("child.db")).is_err());
    }
}
