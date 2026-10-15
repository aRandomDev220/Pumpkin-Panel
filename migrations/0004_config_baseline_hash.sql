-- Phase 4 fix round: durable baseline hash of the last API-saved
-- pumpkin.toml, so crash/out-of-band divergence is detectable on read.

ALTER TABLE server_settings ADD COLUMN content_hash TEXT;
