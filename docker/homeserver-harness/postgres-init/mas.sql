-- The database of the harness's Matrix Authentication Service (Phase 14,
-- ADR 0010): MAS shares Palpo's PostgreSQL sidecar in the harness only (a
-- family pod has its own). Run once, when the data directory is made, by
-- the image's entrypoint; the server trusts every local connection, as
-- Palpo's does.
CREATE ROLE mas LOGIN;
CREATE DATABASE mas OWNER mas;
