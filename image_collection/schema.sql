CREATE TABLE IF NOT EXISTS players (
    id INTEGER PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    rating REAL NOT NULL,
    deviation REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS matches(
    id INTEGER PRIMARY KEY NOT NULL,
    home_players_id INTEGER NOT NULL,
    guest_players_id INTEGER NOT NULL,
    result REAL NOT NULL,
    timestamp DATE NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_player ON players (id);
CREATE INDEX IF NOT EXISTS idx_player_rating ON players (rating); 
CREATE INDEX IF NOT EXISTS idx_player_deviation ON players(deviation);

