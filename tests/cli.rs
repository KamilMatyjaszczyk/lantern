//! Exercise the executable with isolated databases and child-only environments.
use std::{
    fs,
    os::{fd::OwnedFd, unix::net::UnixStream},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
    time::SystemTime,
};

use lantern::{
    model::{SystemSample, unix_timestamp},
    storage::Database,
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "lantern-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.join("state/lantern.db")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lantern"));
        command
            .env("LANTERN_DB", self.path())
            .env("COLUMNS", "40")
            .env("TZ", "UTC");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_and_empty_history_explain_collection_without_creating_directories() {
    let fixture = Fixture::new();
    for option in ["--graph", "-graph"] {
        let output = fixture.command().arg(option).output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "No historical Lantern data is available yet.\nStart the collector with:\n\n    lantern --collect\n"
        );
        assert!(!fixture.path().parent().unwrap().exists());
    }
    drop(Database::open(fixture.path()).unwrap());
    let output = fixture.command().arg("--graph").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("No historical Lantern data")
    );
}

#[test]
fn graph_uses_only_last_day_and_honours_width_timezone_and_alias() {
    let fixture = Fixture::new();
    let mut database = Database::open(fixture.path()).unwrap();
    let now = unix_timestamp(SystemTime::now()).unwrap();
    let mut samples = Vec::new();
    for (age, cpu) in [(90_000, 99.0), (300, 10.0), (250, 30.0)] {
        let mut sample = SystemSample::new(now - age);
        sample.cpu_percent = Some(cpu);
        sample.memory_used = Some(50);
        sample.memory_total = Some(100);
        sample.network_rx_rate = Some(1200.0);
        sample.network_tx_rate = Some(0.0);
        sample.gpu_utilisation = Some(25);
        samples.push(sample);
    }
    database.insert_batch(&samples).unwrap();
    drop(database);
    let before = fs::read(fixture.path()).unwrap();
    for option in ["--graph", "-graph"] {
        let output = fixture.command().arg(option).output().unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("2 samples"));
        assert!(text.contains("avg 20.0 %"));
        assert!(text.contains("peak 30.0 %"));
        assert!(text.contains("GPU %"));
        assert!(text.contains("+0000"));
        assert!(text.lines().all(|line| line.chars().count() <= 40));
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
        let first = chrono::DateTime::from_timestamp(now - 300, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S +0000")
            .to_string();
        let last = chrono::DateTime::from_timestamp(now - 250, 0)
            .unwrap()
            .format("%Y-%m-%d %H:%M:%S +0000")
            .to_string();
        assert!(text.contains(&format!("From {first}")));
        assert!(text.contains(&format!("To   {last}")));
        assert!(!text.contains("Q / Esc"));
    }
    let output = fixture
        .command()
        .env("TZ", "Europe/Oslo")
        .arg("--graph")
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("+0100") || text.contains("+0200"));
}

#[test]
fn graph_anchors_to_saved_history_even_when_collection_stopped_days_ago() {
    let fixture = Fixture::new();
    let database = Database::open(fixture.path()).unwrap();
    let mut sample = SystemSample::new(1_700_000_000);
    sample.cpu_percent = Some(10.0);
    database.insert(&sample).unwrap();
    let output = fixture.command().arg("--graph").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("1 samples"));
    assert!(text.contains("2023-11-14 22:13:20 +0000"));
    assert!(text.contains("avg 10.0 %"));
}

#[test]
fn snapshot_does_not_require_a_valid_database_path() {
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .arg("--snapshot")
        .env("LANTERN_DB", "")
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("Lantern"));
    assert!(text.contains("Uptime"));
    assert!(!text.contains('\x1b'));
    assert!(!fixture.path().exists());
}

#[test]
fn corrupt_database_and_invalid_arguments_fail_with_useful_errors() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    fs::write(fixture.path(), b"not a database").unwrap();
    let output = fixture.command().arg("--graph").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("cannot show history"));
    assert!(!error.contains("panicked"));
    let output = fixture.command().arg("--unknown").output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8(output.stderr).unwrap().contains("--help"));
}

#[test]
fn failed_error_output_preserves_exit_status_without_panicking() {
    let fixture = Fixture::new();
    for (argument, code) in [("--unknown", 2), ("--graph", 1)] {
        let full = fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let output = fixture
            .command()
            .env("LANTERN_DB", "")
            .arg(argument)
            .stderr(full)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn closed_output_is_a_clean_exit_and_other_write_errors_are_reported() {
    let fixture = Fixture::new();
    for args in [
        &["--help"][..],
        &["--version"],
        &["--graph"],
        &["--status"],
        &[],
    ] {
        let (reader, writer) = UnixStream::pair().unwrap();
        drop(reader);
        let fd: OwnedFd = writer.into();
        let output = fixture
            .command()
            .args(args)
            .stdout(Stdio::from(fd))
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        assert!(output.stderr.is_empty());
    }
    let output = fixture
        .command()
        .arg("--help")
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot write output")
    );
}

#[test]
fn default_view_builds_events_from_raw_samples_without_writing_the_database() {
    let fixture = Fixture::new();
    let mut db = Database::open(fixture.path()).unwrap();
    let samples: Vec<_> = (0..=60)
        .step_by(5)
        .map(|t| {
            let mut s = SystemSample::new(1_800_000_000 + t);
            s.cpu_percent = Some(95.0);
            s.top_cpu = Some(lantern::model::ProcessUsage {
                pid: 42,
                name: "immich-worker".into(),
                start_ticks: 1,
                usage: 80.0,
            });
            s
        })
        .collect();
    db.insert_batch(&samples).unwrap();
    drop(db);
    let before = fs::read(fixture.path()).unwrap();
    for args in [&[][..], &["--graph"]] {
        let output = fixture
            .command()
            .args(args)
            .env("COLUMNS", "160")
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("Events: 1"));
        assert!(text.contains("High CPU"));
        assert!(text.contains("immich-worker (42)"));
        assert!(text.contains("possible contributor"));
        assert!(text.find("High CPU").unwrap() < text.find("CPU %").unwrap());
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }
}

#[test]
fn temperature_graph_preserves_event_peak_in_celsius_without_changing_history() {
    let fixture = Fixture::new();
    let mut database = Database::open(fixture.path()).unwrap();
    let samples: Vec<_> = (0..=30)
        .step_by(5)
        .map(|time| {
            let mut sample = SystemSample::new(1_800_000_000 + time);
            sample.cpu_percent = Some(20.0);
            sample.cpu_temperature = Some(if time == 15 { 101.6 } else { 81.0 });
            sample
        })
        .collect();
    database.insert_batch(&samples).unwrap();
    drop(database);
    let before = fs::read(fixture.path()).unwrap();
    let output = fixture.command().env("COLUMNS", "120").output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("High CPU temperature"));
    let graph = text.split("CPU temp °C").nth(1).unwrap();
    assert!(graph.contains("peak 101.6 °C"));
    assert!(graph.contains("   110 │"));
    assert!(!graph.contains("101.6 %"));
    assert_eq!(fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn status_reports_missing_and_empty_history_without_creating_files() {
    let fixture = Fixture::new();
    let output = fixture.command().arg("--status").output().unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.starts_with("Lantern · Status"));
    assert!(text.contains("database not created"));
    assert!(text.contains("no saved measurements yet"));
    assert!(!text.contains('\x1b'));
    assert!(!fixture.path().parent().unwrap().exists());
    drop(Database::open(fixture.path()).unwrap());
    let before = fs::read(fixture.path()).unwrap();
    let output = fixture.command().arg("--status").output().unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("no saved measurements yet")
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), before);
}

#[test]
fn status_reports_fresh_stale_and_future_samples_without_modifying_history() {
    for (offset, expected) in [
        (-5, "History    fresh"),
        (-120, "History    stale"),
        (3600, "clock mismatch"),
    ] {
        let fixture = Fixture::new();
        let database = Database::open(fixture.path()).unwrap();
        database
            .insert(&SystemSample::new(
                unix_timestamp(SystemTime::now()).unwrap() + offset,
            ))
            .unwrap();
        drop(database);
        let before = fs::read(fixture.path()).unwrap();
        let output = fixture.command().arg("--status").output().unwrap();
        assert!(output.status.success(), "{:?}", output.stderr);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains(expected), "{text}");
        assert!(text.contains("Collector  not detected"));
        assert!(text.contains("Last saved"));
        assert!(text.contains("+0000"));
        assert!(text.contains("MiB"));
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }
}

#[test]
fn status_rejects_corrupt_history_and_conflicting_modes() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    fs::write(fixture.path(), b"not a database").unwrap();
    let output = fixture.command().arg("--status").output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("cannot show status")
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), b"not a database");
    let output = fixture
        .command()
        .args(["--status", "--graph"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
