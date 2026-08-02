CREATE TABLE IF NOT EXISTS operations (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  plugin_id TEXT NOT NULL,
  state TEXT NOT NULL,
  phase TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  error_code TEXT,
  error_message TEXT,
  created_at_ms INTEGER NOT NULL,
  updated_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS operations_plugin_updated
  ON operations(plugin_id, updated_at_ms DESC);
CREATE TABLE IF NOT EXISTS catalog_state (
  singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
  sequence INTEGER NOT NULL,
  digest TEXT NOT NULL,
  roots_json TEXT NOT NULL,
  accepted_at_ms INTEGER NOT NULL
);
