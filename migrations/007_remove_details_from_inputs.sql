-- Remove details column from inputs table since it's redundant
-- The information in details is already available through input_type + assigned_port + name

-- Remove details column from inputs table
ALTER TABLE inputs DROP COLUMN details;