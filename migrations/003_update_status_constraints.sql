-- Update status constraints to support granular connection states
-- This migration updates the CHECK constraints to support the new connection states

-- SQLite doesn't support modifying CHECK constraints directly, so we need to recreate the tables

-- Step 1: Create new tables with updated constraints
CREATE TABLE inputs_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,
    kind TEXT NOT NULL,
    config_json TEXT NOT NULL,
    details TEXT NOT NULL,
    status TEXT DEFAULT 'stopped' CHECK (status IN ('stopped', 'listening', 'connecting', 'connected', 'reconnecting', 'error'))
);

CREATE TABLE outputs_new (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT,
    input_id INTEGER NOT NULL,
    kind TEXT NOT NULL,
    destination TEXT,
    config_json TEXT,
    listen_port INTEGER,
    status TEXT DEFAULT 'stopped' CHECK (status IN ('stopped', 'listening', 'connecting', 'connected', 'reconnecting', 'error')),
    FOREIGN KEY (input_id) REFERENCES inputs_new (id) ON DELETE CASCADE
);

-- Step 2: Copy data from old tables, mapping old states to new ones
INSERT INTO inputs_new (id, name, kind, config_json, details, status)
SELECT id, name, kind, config_json, details,
    CASE
        WHEN status = 'running' THEN 'connected'
        WHEN status = 'stopped' THEN 'stopped'
        ELSE status
    END
FROM inputs;

INSERT INTO outputs_new (id, name, input_id, kind, destination, config_json, listen_port, status)
SELECT id, name, input_id, kind, destination, config_json, listen_port,
    CASE
        WHEN status = 'running' THEN 'connected'
        WHEN status = 'stopped' THEN 'stopped'
        ELSE status
    END
FROM outputs;

-- Step 3: Drop old tables and rename new ones
DROP TABLE outputs;
DROP TABLE inputs;
ALTER TABLE inputs_new RENAME TO inputs;
ALTER TABLE outputs_new RENAME TO outputs;

-- Step 4: Recreate indexes
CREATE INDEX IF NOT EXISTS idx_inputs_status ON inputs (status);
CREATE INDEX IF NOT EXISTS idx_outputs_status ON outputs (status);