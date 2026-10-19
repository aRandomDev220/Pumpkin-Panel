-- Reserve both the Java game port and the derived Bedrock port
-- (`game_port + 1`) for every live server. The primary key is the
-- concurrency backstop: two creates that pick overlapping Java/Bedrock
-- pairs cannot both commit.
CREATE TABLE IF NOT EXISTS server_port_reservations (
    port INTEGER PRIMARY KEY,
    server_id TEXT NOT NULL REFERENCES servers(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_server_port_reservations_server
    ON server_port_reservations(server_id);

-- Backfill existing live servers. `OR IGNORE` keeps the migration safe on a
-- legacy database that already contains an adjacent-port overlap, while new
-- writes are still protected by the primary key.
INSERT OR IGNORE INTO server_port_reservations (port, server_id)
    SELECT game_port, id FROM servers WHERE deleted_at IS NULL;

INSERT OR IGNORE INTO server_port_reservations (port, server_id)
    SELECT game_port + 1, id FROM servers WHERE deleted_at IS NULL;
