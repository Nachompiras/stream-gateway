-- Remove assigned_port columns since ports are now stored in configuration JSON
-- This simplifies the schema to have a single source of truth

-- First, drop the indexes that were created for assigned_port (must be done before dropping columns)
DROP INDEX IF EXISTS idx_inputs_assigned_port;
DROP INDEX IF EXISTS idx_outputs_assigned_port;

-- Remove assigned_port from inputs table
ALTER TABLE inputs DROP COLUMN assigned_port;

-- Remove assigned_port from outputs table
ALTER TABLE outputs DROP COLUMN assigned_port;