CREATE TABLE samples (
    id INTEGER PRIMARY KEY,
    timestamp INTEGER NOT NULL,
    cpu_percent REAL CHECK (cpu_percent BETWEEN 0 AND 100),
    load_1 REAL CHECK (load_1 >= 0),
    load_5 REAL CHECK (load_5 >= 0),
    load_15 REAL CHECK (load_15 >= 0),
    memory_used INTEGER CHECK (memory_used >= 0),
    memory_total INTEGER CHECK (memory_total > 0),
    swap_used INTEGER CHECK (swap_used >= 0),
    swap_total INTEGER CHECK (swap_total >= 0),
    network_rx_rate REAL CHECK (network_rx_rate >= 0),
    network_tx_rate REAL CHECK (network_tx_rate >= 0),
    disk_read_rate REAL CHECK (disk_read_rate >= 0),
    disk_write_rate REAL CHECK (disk_write_rate >= 0),
    cpu_temperature REAL CHECK (cpu_temperature >= -273.15),
    gpu_utilisation INTEGER CHECK (gpu_utilisation BETWEEN 0 AND 100),
    gpu_memory_used INTEGER CHECK (gpu_memory_used >= 0),
    gpu_memory_total INTEGER CHECK (gpu_memory_total > 0),
    gpu_temperature INTEGER CHECK (gpu_temperature BETWEEN 0 AND 250),
    CHECK (memory_used <= memory_total),
    CHECK (swap_used <= swap_total),
    CHECK (gpu_memory_used <= gpu_memory_total)
) STRICT;

CREATE INDEX samples_timestamp ON samples(timestamp);
PRAGMA user_version = 1;
