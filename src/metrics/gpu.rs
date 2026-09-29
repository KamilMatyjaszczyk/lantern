#[derive(Debug, PartialEq, Eq)]
pub struct GpuMemory {
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct GpuMetrics {
    pub utilisation: Option<u32>,
    pub memory: Option<GpuMemory>,
    pub temperature: Option<u32>,
}

impl GpuMetrics {
    #[cfg(any(feature = "nvidia", test))]
    fn from_readings(
        utilisation: Option<u32>,
        memory: Option<GpuMemory>,
        temperature: Option<u32>,
    ) -> Option<Self> {
        let utilisation = utilisation.filter(|value| *value <= 100);
        let memory =
            memory.filter(|value| value.total_bytes > 0 && value.used_bytes <= value.total_bytes);
        let temperature = temperature.filter(|value| *value <= 250);
        if utilisation.is_none() && memory.is_none() && temperature.is_none() {
            return None;
        }
        Some(Self {
            utilisation,
            memory,
            temperature,
        })
    }
}

pub struct GpuCollector {
    #[cfg(feature = "nvidia")]
    nvml: nvml_wrapper::Nvml,
    #[cfg(feature = "nvidia")]
    uuid: String,
}

#[cfg(feature = "nvidia")]
impl GpuCollector {
    pub fn new() -> Option<Self> {
        // Use the runtime SONAME; the unversioned .so may require a dev package.
        Self::from_library(std::ffi::OsStr::new("libnvidia-ml.so.1"))
    }

    fn from_library(path: &std::ffi::OsStr) -> Option<Self> {
        let nvml = nvml_wrapper::Nvml::builder().lib_path(path).init().ok()?;
        // V1 monitors the first accessible GPU. Retain its UUID so a later
        // sample cannot silently switch to another GPU if indices change.
        let uuid = (0..nvml.device_count().ok()?)
            .find_map(|index| nvml.device_by_index(index).ok()?.uuid().ok())?;
        Some(Self { nvml, uuid })
    }

    pub fn sample(&self) -> Option<GpuMetrics> {
        use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

        // Retain the NVML instance for repeated sampling; dropping it shuts
        // NVML down. No subprocesses or driver/header dependencies at build time.
        let device = self.nvml.device_by_uuid(self.uuid.as_str()).ok()?;
        GpuMetrics::from_readings(
            device.utilization_rates().ok().map(|rates| rates.gpu),
            device.memory_info().ok().map(|memory| GpuMemory {
                used_bytes: memory.used,
                total_bytes: memory.total,
            }),
            device.temperature(TemperatureSensor::Gpu).ok(),
        )
    }
}

#[cfg(not(feature = "nvidia"))]
impl GpuCollector {
    pub fn new() -> Option<Self> {
        None
    }

    pub fn sample(&self) -> Option<GpuMetrics> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_valid_readings_in_bytes_percent_and_celsius() {
        let memory = GpuMemory {
            used_bytes: 2 * 1024_u64.pow(3),
            total_bytes: 8 * 1024_u64.pow(3),
        };
        let metrics = GpuMetrics::from_readings(Some(25), Some(memory), Some(44)).unwrap();
        assert_eq!(metrics.utilisation, Some(25));
        assert_eq!(metrics.temperature, Some(44));
        assert_eq!(metrics.memory.unwrap().used_bytes, 2 * 1024_u64.pow(3));
    }

    #[test]
    fn unsupported_metric_does_not_hide_other_readings() {
        let metrics = GpuMetrics::from_readings(
            None,
            Some(GpuMemory {
                used_bytes: 0,
                total_bytes: 8 * 1024_u64.pow(3),
            }),
            Some(44),
        )
        .unwrap();
        assert_eq!(metrics.utilisation, None);
        assert_eq!(metrics.memory.unwrap().used_bytes, 0);
        assert_eq!(metrics.temperature, Some(44));
        let metrics = GpuMetrics::from_readings(Some(0), None, None).unwrap();
        assert_eq!(metrics.utilisation, Some(0));
        assert_eq!(metrics.memory, None);
        assert_eq!(metrics.temperature, None);
    }

    #[test]
    fn entirely_unavailable_gpu_is_omitted() {
        assert_eq!(GpuMetrics::from_readings(None, None, None), None);
    }

    #[test]
    fn rejects_invalid_values_without_fake_defaults() {
        assert_eq!(
            GpuMetrics::from_readings(
                Some(101),
                Some(GpuMemory {
                    used_bytes: 200,
                    total_bytes: 100,
                }),
                Some(u32::MAX)
            ),
            None
        );
        let metrics = GpuMetrics::from_readings(
            Some(100),
            Some(GpuMemory {
                used_bytes: 0,
                total_bytes: 0,
            }),
            None,
        )
        .unwrap();
        assert_eq!(metrics.utilisation, Some(100));
        assert_eq!(metrics.memory, None);
    }

    #[cfg(feature = "nvidia")]
    #[test]
    fn missing_nvml_library_disables_gpu_without_panicking() {
        // /dev/null cannot be a directory; independent of installed hardware.
        assert!(
            GpuCollector::from_library(std::ffi::OsStr::new("/dev/null/lantern-missing-nvml.so"))
                .is_none()
        );
    }

    #[cfg(not(feature = "nvidia"))]
    #[test]
    fn disabled_feature_never_initialises_gpu() {
        assert!(GpuCollector::new().is_none());
    }
}
