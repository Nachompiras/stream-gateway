-- Unified database schema for SRT Gateway
-- This migration consolidates all previous migrations into a single file

-- Table for storing input stream configurations
CREATE TABLE IF NOT EXISTS inputs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,                        -- Human readable name/description
    kind TEXT NOT NULL,               -- 'udp', 'srt_listener', 'srt_caller'
    config_json TEXT NOT NULL,        -- JSON serialized configuration
    details TEXT NOT NULL,            -- Human readable description
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- Table for storing output stream configurations
CREATE TABLE IF NOT EXISTS outputs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    input_id INTEGER NOT NULL,        -- References inputs.id
    name TEXT,                        -- Human readable name/description
    kind TEXT NOT NULL,               -- 'udp', 'srt_caller', 'srt_listener'
    destination TEXT NOT NULL,        -- Target address/endpoint
    config_json TEXT,                 -- JSON serialized configuration (optional)
    listen_port INTEGER,              -- Port for SRT Listener outputs
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (input_id) REFERENCES inputs (id) ON DELETE CASCADE
);

-- Index for better performance on input_id lookups
CREATE INDEX IF NOT EXISTS idx_outputs_input_id ON outputs (input_id);