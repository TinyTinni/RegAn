CREATE TABLE IF NOT EXISTS players (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT UNIQUE NOT NULL,
    rating REAL NOT NULL,
    deviation REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS matches(
    id INTEGER PRIMARY KEY NOT NULL,
    home_players_id INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    guest_players_id INTEGER NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    result REAL NOT NULL CHECK (result IN (0.0, 0.5, 1.0)),
    timestamp TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_player_rating ON players (rating); 
CREATE INDEX IF NOT EXISTS idx_player_deviation ON players(deviation);

