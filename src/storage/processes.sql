-- Add nullable evidence without rewriting or discarding existing history.
ALTER TABLE samples ADD COLUMN top_cpu_pid INTEGER CHECK (top_cpu_pid > 0);
ALTER TABLE samples ADD COLUMN top_cpu_name TEXT;
ALTER TABLE samples ADD COLUMN top_cpu_start INTEGER CHECK (top_cpu_start >= 0);
ALTER TABLE samples ADD COLUMN top_cpu_usage REAL CHECK (top_cpu_usage BETWEEN 0 AND 100);
ALTER TABLE samples ADD COLUMN top_memory_pid INTEGER CHECK (top_memory_pid > 0);
ALTER TABLE samples ADD COLUMN top_memory_name TEXT;
ALTER TABLE samples ADD COLUMN top_memory_start INTEGER CHECK (top_memory_start >= 0);
ALTER TABLE samples ADD COLUMN top_memory_usage REAL CHECK (top_memory_usage >= 0);
PRAGMA user_version = 2;
