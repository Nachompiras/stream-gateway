-- Add status column to inputs and outputs tables
-- This allows for start/stop control without deleting streams

-- Add status column to inputs table
ALTER TABLE inputs ADD COLUMN status TEXT DEFAULT 'running' CHECK (status IN ('running', 'stopped'));

-- Add status column to outputs table
ALTER TABLE outputs ADD COLUMN status TEXT DEFAULT 'running' CHECK (status IN ('running', 'stopped'));

-- Create indexes for better performance when filtering by status
CREATE INDEX IF NOT EXISTS idx_inputs_status ON inputs (status);
CREATE INDEX IF NOT EXISTS idx_outputs_status ON outputs (status);