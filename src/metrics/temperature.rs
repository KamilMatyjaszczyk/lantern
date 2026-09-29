use std::fs;
use std::path::{Path, PathBuf};

pub fn read_cpu_temperature() -> Option<f64> {
    read_from(
        Path::new("/sys/class/hwmon"),
        Path::new("/sys/class/thermal"),
    )
}

fn read_from(hwmon: &Path, thermal: &Path) -> Option<f64> {
    let mut sensors: Vec<(u8, PathBuf)> = Vec::new();
    if let Ok(devices) = fs::read_dir(hwmon) {
        for device in devices.flatten() {
            let path = device.path();
            let Ok(name) = fs::read_to_string(path.join("name")) else {
                continue;
            };
            if !matches!(name.trim(), "coretemp" | "k10temp" | "zenpower") {
                continue;
            }
            let Ok(files) = fs::read_dir(&path) else {
                continue;
            };
            for file in files.flatten() {
                let filename = file.file_name();
                let Some(stem) = filename.to_str().and_then(temperature_stem) else {
                    continue;
                };
                let label =
                    fs::read_to_string(path.join(format!("{stem}_label"))).unwrap_or_default();
                // Prefer package/die readings. Tctl is the AMD control reading
                // and can include a model-specific offset from die temperature.
                let priority = match label.trim() {
                    "Tdie" => 0,
                    label if label.starts_with("Package id ") => 0,
                    "Tctl" => 1,
                    _ => 2,
                };
                sensors.push((priority, file.path()));
            }
        }
    }
    if let Ok(zones) = fs::read_dir(thermal) {
        for zone in zones.flatten() {
            let path = zone.path();
            let Ok(kind) = fs::read_to_string(path.join("type")) else {
                continue;
            };
            // Orange Pi 4 Pro's Allwinner A733 exposes separate little/big
            // CPU cluster zones. Match their types, not unstable zone numbers;
            // the hottest valid CPU zone wins at this priority. GPU, NPU and
            // memory zones must never be substituted for CPU temperature.
            if matches!(
                kind.trim(),
                "x86_pkg_temp"
                    | "cpu-thermal"
                    | "cpu_thermal"
                    | "cpul_thermal_zone"
                    | "cpub_thermal_zone"
            ) {
                sensors.push((3, path.join("temp")));
            }
        }
    }
    sensors.sort_by_key(|(priority, _)| *priority);
    let mut selected_priority = None;
    let mut hottest: Option<f64> = None;
    for (priority, path) in sensors {
        if selected_priority.is_some_and(|selected| priority > selected) {
            break;
        }
        if let Some(value) = fs::read_to_string(path)
            .ok()
            .and_then(|text| parse_temperature(&text))
        {
            selected_priority = Some(priority);
            hottest = Some(hottest.map_or(value, |previous| previous.max(value)));
        }
    }
    hottest
}

fn temperature_stem(filename: &str) -> Option<&str> {
    let stem = filename.strip_suffix("_input")?;
    let index = stem.strip_prefix("temp")?;
    (!index.is_empty() && index.bytes().all(|byte| byte.is_ascii_digit())).then_some(stem)
}

fn parse_temperature(text: &str) -> Option<f64> {
    let millidegrees = text.trim().parse::<i64>().ok()?;
    // Reject error sentinels and readings outside a plausible CPU sensor range.
    (-273_150..=250_000)
        .contains(&millidegrees)
        .then_some(millidegrees as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lantern-temperature-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, file: &str, text: &str) {
            let path = self.0.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn read(&self) -> Option<f64> {
            read_from(&self.0.join("hwmon"), &self.0.join("thermal"))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn converts_millidegrees_and_rejects_invalid_values() {
        assert_eq!(parse_temperature("48500\n"), Some(48.5));
        assert_eq!(parse_temperature("0"), Some(0.0));
        for text in ["", "N/A", "NaN", "-274000", "999999999"] {
            assert_eq!(parse_temperature(text), None);
        }
    }

    #[test]
    fn absent_or_unrelated_sensors_are_unavailable() {
        let fixture = Fixture::new();
        assert_eq!(fixture.read(), None);
        fixture.write("hwmon/hwmon0/name", "nvme\n");
        fixture.write("hwmon/hwmon0/temp1_input", "45000");
        fixture.write("thermal/thermal_zone0/type", "acpitz");
        fixture.write("thermal/thermal_zone0/temp", "65000");
        assert_eq!(fixture.read(), None);
    }

    #[test]
    fn prefers_package_over_individual_cores() {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "coretemp\n");
        fixture.write("hwmon/hwmon0/temp1_label", "Package id 0\n");
        fixture.write("hwmon/hwmon0/temp1_input", "48000");
        fixture.write("hwmon/hwmon0/temp2_label", "Core 0");
        fixture.write("hwmon/hwmon0/temp2_input", "52000");
        assert_eq!(fixture.read(), Some(48.0));
    }

    #[test]
    fn prefers_die_over_control_and_falls_back_when_unreadable() {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "k10temp");
        fixture.write("hwmon/hwmon0/temp1_label", "Tctl");
        fixture.write("hwmon/hwmon0/temp1_input", "65000");
        fixture.write("hwmon/hwmon0/temp2_label", "Tdie");
        fixture.write("hwmon/hwmon0/temp2_input", "45000");
        assert_eq!(fixture.read(), Some(45.0));
        fixture.write("hwmon/hwmon0/temp2_input", "unavailable");
        assert_eq!(fixture.read(), Some(65.0));
    }

    #[test]
    fn falls_back_to_cpu_thermal_zone() {
        let fixture = Fixture::new();
        fixture.write("hwmon/hwmon0/name", "coretemp");
        fixture.write("hwmon/hwmon0/temp1_input", "invalid");
        fixture.write("thermal/thermal_zone0/type", "cpu-thermal\n");
        fixture.write("thermal/thermal_zone0/temp", "51000\n");
        assert_eq!(fixture.read(), Some(51.0));
    }

    #[test]
    fn orange_pi_4_pro_uses_hottest_cpu_cluster_regardless_of_zone_number() {
        let fixture = Fixture::new();
        fixture.write("thermal/thermal_zone7/type", "cpul_thermal_zone\n");
        fixture.write("thermal/thermal_zone7/temp", "54925\n");
        fixture.write("thermal/thermal_zone2/type", "cpub_thermal_zone\n");
        fixture.write("thermal/thermal_zone2/temp", "61990\n");
        for (index, kind) in ["gpu_thermal_zone", "npu_thermal_zone", "ddr_thermal_zone"]
            .into_iter()
            .enumerate()
        {
            fixture.write(&format!("thermal/thermal_zone{index}0/type"), kind);
            fixture.write(&format!("thermal/thermal_zone{index}0/temp"), "95000");
        }
        assert_eq!(fixture.read(), Some(61.99));
        fixture.write("thermal/thermal_zone7/temp", "65000");
        assert_eq!(fixture.read(), Some(65.0));
    }

    #[test]
    fn orange_pi_4_pro_keeps_valid_cluster_when_other_is_missing_or_invalid() {
        let fixture = Fixture::new();
        fixture.write("thermal/thermal_zone0/type", "cpul_thermal_zone");
        fixture.write("thermal/thermal_zone0/temp", "54925");
        fixture.write("thermal/thermal_zone1/type", "cpub_thermal_zone");
        assert_eq!(fixture.read(), Some(54.925));
        for invalid in ["unavailable", "999999999", "-274000"] {
            fixture.write("thermal/thermal_zone1/temp", invalid);
            assert_eq!(fixture.read(), Some(54.925));
        }
        fixture.write("thermal/thermal_zone0/temp", "unavailable");
        fixture.write("thermal/thermal_zone2/type", "gpu_thermal_zone");
        fixture.write("thermal/thermal_zone2/temp", "62000");
        assert_eq!(fixture.read(), None);
    }
}
