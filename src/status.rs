//! One-shot, read-only status. No sampling, event analysis or background work.
use std::{
    error::Error,
    fs,
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    time::SystemTime,
};

use chrono::{DateTime, Local};
use lantern::{
    model::unix_timestamp,
    storage::{Database, default_db_path},
};

const FRESH_SECONDS: i64 = 60; // Allow two normal 30-second commit intervals.

pub fn run(output: &mut impl Write) -> Result<(), Box<dyn Error>> {
    let path = std::env::current_dir()?.join(default_db_path()?);
    let database = Database::open_existing(&path)?;
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let bounds = database
        .as_ref()
        .map(Database::timestamp_bounds)
        .transpose()?
        .flatten();
    writeln!(output, "Lantern · Status")?;
    writeln!(output, "Database   {}", path.display())?;
    match collector_pids(metadata.as_ref(), Path::new("/proc")) {
        Ok(pids) if !pids.is_empty() => {
            let pids = pids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(output, "Collector  detected · PID {pids} · same database")?;
        }
        Ok(_) => writeln!(output, "Collector  not detected among visible processes")?,
        Err(_) => writeln!(
            output,
            "Collector  unknown · process information unavailable"
        )?,
    }
    if let Some(metadata) = metadata {
        writeln!(
            output,
            "Size       {:.2} MiB ({} bytes)",
            metadata.len() as f64 / 1_048_576.0,
            metadata.len()
        )?;
    } else {
        writeln!(output, "Size       database not created")?;
    }
    if let Some((first, last)) = bounds {
        writeln!(output, "First      {}", local_time(first))?;
        writeln!(output, "Last saved {}", local_time(last))?;
        writeln!(
            output,
            "History    {}",
            freshness(last, unix_timestamp(SystemTime::now())?)
        )?;
    } else {
        writeln!(output, "History    no saved measurements yet")?;
    }
    writeln!(
        output,
        "Saves about every 30s; recent samples may still be buffered."
    )?;
    writeln!(output, "Collection: lantern --collect")?;
    Ok(())
}

fn local_time(timestamp: i64) -> String {
    DateTime::from_timestamp(timestamp, 0)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S %z")
                .to_string()
        })
        .unwrap_or_else(|| "out-of-range timestamp".into())
}

fn freshness(last: i64, now: i64) -> String {
    let age = i128::from(now) - i128::from(last);
    if age < 0 {
        return format!("clock mismatch · last sample is {}s in the future", -age);
    }
    if age <= i128::from(FRESH_SECONDS) {
        format!("fresh · last saved sample {age}s ago")
    } else {
        format!("stale · last saved sample {age}s ago (over {FRESH_SECONDS}s)")
    }
}

/// Best-effort detection without a pidfile or collector heartbeat. Verify both
/// the executable/arguments and an open descriptor for this database's inode.
/// A fresh final flush alone must never be described as a running collector.
fn collector_pids(database: Option<&fs::Metadata>, proc_root: &Path) -> io::Result<Vec<u32>> {
    let Some(database) = database else {
        return Ok(Vec::new());
    };
    let executable = std::env::current_exe()?;
    let current = fs::metadata(&executable)?;
    let expected_name = executable.file_name().unwrap_or_default().to_string_lossy();
    let mut pids = Vec::new();
    for entry in fs::read_dir(proc_root)?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let process = entry.path();
        let Ok(args) = fs::read(process.join("cmdline")) else {
            continue;
        };
        let mut args = args.split(|byte| *byte == 0);
        let _program = args.next();
        if args.next() != Some(b"--collect".as_slice()) || args.any(|arg| !arg.is_empty()) {
            continue;
        }
        let Ok(target) = fs::read_link(process.join("exe")) else {
            continue;
        };
        let same_executable =
            fs::metadata(process.join("exe")).is_ok_and(|m| same_file(&m, &current));
        let name = target.file_name().unwrap_or_default().to_string_lossy();
        // Still recognise a collector started before the binary was rebuilt.
        if !same_executable && name.trim_end_matches(" (deleted)") != expected_name {
            continue;
        }
        let Ok(descriptors) = fs::read_dir(process.join("fd")) else {
            continue;
        };
        if descriptors
            .flatten()
            .any(|fd| fs::metadata(fd.path()).is_ok_and(|m| same_file(&m, database)))
        {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

fn same_file(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn freshness_handles_commit_delay_stale_history_and_clock_changes() {
        assert_eq!(freshness(100, 160), "fresh · last saved sample 60s ago");
        assert!(freshness(100, 161).starts_with("stale"));
        assert!(freshness(101, 100).contains("1s in the future"));
        assert!(freshness(i64::MIN, i64::MAX).starts_with("stale"));
        assert!(freshness(i64::MAX, i64::MIN).starts_with("clock mismatch"));
    }

    #[test]
    fn detection_requires_collector_arguments_and_the_same_open_database() {
        let root = std::env::temp_dir().join(format!("lantern-status-proc-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let db = root.join("history.db");
        let other = root.join("other.db");
        fs::write(&db, []).unwrap();
        fs::write(&other, []).unwrap();
        for (pid, argument, target) in [
            (1, "--collect", &db),
            (2, "--collect", &other),
            (3, "--graph", &db),
        ] {
            let process = root.join(pid.to_string());
            fs::create_dir_all(process.join("fd")).unwrap();
            fs::write(process.join("cmdline"), format!("lantern\0{argument}\0")).unwrap();
            symlink(std::env::current_exe().unwrap(), process.join("exe")).unwrap();
            symlink(target, process.join("fd/7")).unwrap();
        }
        assert_eq!(
            collector_pids(Some(&fs::metadata(&db).unwrap()), &root).unwrap(),
            vec![1]
        );
        assert!(collector_pids(None, &root).unwrap().is_empty());
        fs::remove_file(root.join("1/fd/7")).unwrap();
        assert!(
            collector_pids(Some(&fs::metadata(&db).unwrap()), &root)
                .unwrap()
                .is_empty()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
