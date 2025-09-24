-- Add naming consistency for host/port fields
-- This migration documents the transition to consistent host/port naming
-- The actual compatibility is handled in the application code through helper functions

-- This migration adds comments and doesn't change the schema since the application
-- now supports both old and new field naming through helper functions

-- Old naming scheme supported (for backward compatibility):
--   - listen_port (UDP inputs, SRT listener inputs/outputs)  
--   - target_addr (SRT caller inputs)
--   - destination_addr (UDP outputs, SRT caller outputs)

-- New naming scheme (preferred):
--   - bind_host + bind_port (for listeners)
--   - remote_host + remote_port (for callers/destinations)

-- The application code uses helper methods like:
--   - get_bind_host() / get_bind_port() for listener configurations
--   - get_remote_host() / get_remote_port() for caller/destination configurations

-- No schema changes needed - all handled in application layer for backward compatibility

-- Add comments to document the naming transition
PRAGMA table_info(inputs);
PRAGMA table_info(outputs);

-- This migration serves as documentation of the naming consistency improvement
-- All existing data continues to work through the compatibility layer in models.rs