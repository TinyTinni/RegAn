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