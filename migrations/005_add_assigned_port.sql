-- Add assigned_port columns to track automatically assigned ports
-- This allows the API to return the port that was automatically selected

-- Add assigned_port to inputs table (for UDP listeners and SRT listeners)
ALTER TABLE inputs ADD COLUMN assigned_port INTEGER;

-- Add assigned_port to outputs table (for SRT listener outputs and UDP outputs with auto port)
ALTER TABLE outputs ADD COLUMN assigned_port INTEGER;

-- Create index on assigned_port for port conflict checking
CREATE INDEX IF NOT EXISTS idx_inputs_assigned_port ON inputs (assigned_port);
CREATE INDEX IF NOT EXISTS idx_outputs_assigned_port ON outputs (assigned_port);