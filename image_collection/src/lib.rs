#[macro_use]
mod glicko;

use anyhow::Result;
use crossbeam_queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::collections::HashSet;
use std::str::FromStr;
use tokio_stream::StreamExt;

use rand::prelude::*;
use tracing::*;

#[derive(Clone)]
pub struct ImageCollection {
    /// buffers pre-computed matches
    candidates: std::sync::Arc<ArrayQueue<Duel>>,
    /// is true, when a thread is already in process in filling the queue up
    db_update_in_progress: std::sync::Arc<std::sync::atomic::AtomicBool>,

    /// matchmaking strategy used for upcoming duels
    strategy: Strategy,
    /// single source of randomness for everything this collection does
    rng: std::sync::Arc<std::sync::Mutex<StdRng>>,

    db: SqlitePool,
}

/// Strategy used to pick an opponent for a player.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    /// Pick a random opponent whose rating lies within
    /// `rating ± 0.96 * deviation` of the player.
    /// Falls back to a uniformly random opponent when the window is empty.
    #[default]
    Windowed,
    /// Pick a uniformly random opponent, ignoring ratings entirely.
    Uniform,
}

impl FromStr for Strategy {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "windowed" => Ok(Strategy::Windowed),
            "uniform" => Ok(Strategy::Uniform),
            other => Err(format!(
                "unknown strategy {other:?}, expected \"windowed\" or \"uniform\""
            )),
        }
    }
}

const WINDOW_FACTOR: f64 = 0.96;
const RECENT_OPPONENTS: i64 = 3;

fn batch_rng(shared: &std::sync::Arc<std::sync::Mutex<StdRng>>) -> StdRng {
    let mut shared = shared.lock().expect("rng mutex poisoned");
    StdRng::from_rng(&mut *shared)
}

/// Options to create an ImageCollection Type
pub struct ImageCollectionOptions {
    /// path to the sqlite db
    pub db_path: String,
    /// size of the pre-computation buffer
    /// the buffer holds possible candidates in a prio queue.
    /// The best matchmakings gets decided at some point
    /// and is not getting updated until the queue gets filled again,
    /// leading to a possible worse machtmaking with a too huge caching.
    /// It is used to reduce the queries to the db.
    pub candidate_buffer: usize,
}

/// a new match which needs to be played
#[derive(Serialize, Deserialize)]
pub struct Duel {
    pub home: String,
    pub home_id: u32,
    pub guest: String,
    pub guest_id: u32,
}

/// The result of a played match in terms of the home player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MatchOutcome {
    HomeWin,
    Draw,
    GuestWin,
}

impl From<MatchOutcome> for f32 {
    fn from(o: MatchOutcome) -> f32 {
        match o {
            MatchOutcome::HomeWin => 1.0,
            MatchOutcome::Draw => 0.5,
            MatchOutcome::GuestWin => 0.0,
        }
    }
}

impl From<MatchOutcome> for f64 {
    fn from(o: MatchOutcome) -> f64 {
        match o {
            MatchOutcome::HomeWin => 1.0,
            MatchOutcome::Draw => 0.5,
            MatchOutcome::GuestWin => 0.0,
        }
    }
}

impl TryFrom<f32> for MatchOutcome {
    type Error = &'static str;
    fn try_from(v: f32) -> Result<Self, Self::Error> {
        if v == 1.0 {
            Ok(MatchOutcome::HomeWin)
        } else if v == 0.5 {
            Ok(MatchOutcome::Draw)
        } else if v == 0.0 {
            Ok(MatchOutcome::GuestWin)
        } else {
            Err("match outcome must be 0.0, 0.5, or 1.0")
        }
    }
}

impl Serialize for MatchOutcome {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let v: f32 = (*self).into();
        v.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MatchOutcome {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let v = f32::deserialize(deserializer)?;
        Self::try_from(v).map_err(serde::de::Error::custom)
    }
}

/// a played match with the given result
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Match {
    pub home_id: u32,
    pub guest_id: u32,
    /// 0 if home lost, 0.5 on draw, 1 if home won
    pub won: MatchOutcome,
}

impl ImageCollection {
    pub async fn close(&self) {
        self.db.close().await //async, can therefore not implemented in drop
    }

    pub async fn new(
        options: &ImageCollectionOptions,
        image_dir: &String,
    ) -> Result<ImageCollection> {
        let db_opions = sqlx::sqlite::SqliteConnectOptions::from_str(&options.db_path)?
            .shared_cache(false)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5000))
            .locking_mode(sqlx::sqlite::SqliteLockingMode::Exclusive)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true)
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "134217728")
            .pragma("journal_size_limit", "67108864")
            .pragma("cache_size", "64000")
            .create_if_missing(true);

        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(db_opions)
            .await?;
        sqlx::query_file!("./schema.sql").execute(&db).await?;
        check_db_integrity(&db, image_dir).await?;
        let (max_players,): (i64,) = sqlx::query_as("SELECT COUNT(*) as count FROM players")
            .fetch_one(&db)
            .await?;
        let max_players = max_players as usize;

        let candidate_buffer = {
            if max_players > options.candidate_buffer {
                options.candidate_buffer
            } else if options.candidate_buffer >= 3 {
                let new_buffer = std::cmp::min(3, max_players);
                warn!(
                    "Max players exceeds candidate buffer. Lowering candidate buffer to {}. Player count: {}",
                    new_buffer, max_players
                );
                new_buffer
            } else {
                1
            }
        };

        let candidates = ArrayQueue::<Duel>::new(std::cmp::max(1, candidate_buffer));
        let candidates = std::sync::Arc::new(candidates);
        let rng = std::sync::Arc::new(std::sync::Mutex::new(rand::make_rng::<StdRng>()));
        let mut batch = batch_rng(&rng);
        let new_duels =
            calculate_new_matches(&db, candidates.capacity(), Strategy::default(), &mut batch)
                .await?;
        for nd in new_duels.into_iter() {
            let _ = candidates.push(nd);
        }
        let db_update_in_progress = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        Ok(ImageCollection {
            candidates,
            db,
            db_update_in_progress,
            strategy: Strategy::default(),
            rng,
        })
    }

    pub async fn msre(&self) -> Result<f32> {
        struct Player {
            name: String,
        }

        let players = sqlx::query_as!(Player, "SELECT name FROM players ORDER BY rating")
            .fetch_all(&self.db)
            .await?;
        let mut sqre = 0_f32;
        let mut len = 0_f32;
        for (i, rank) in players
            .iter()
            .filter_map(|player| player.name.parse::<f32>().ok())
            .enumerate()
        {
            let rankdiff = rank - i as f32;
            sqre += rankdiff * rankdiff;
            len += 1_f32;
        }
        sqre = (sqre / len).sqrt();
        Ok(sqre)
    }

    pub async fn print_csv(&self) -> Result<()> {
        struct Player {
            name: String,
            rating: f64,
            deviation: f64,
        }

        let players = sqlx::query_as!(
            Player,
            "SELECT name, rating, deviation FROM players ORDER BY rating"
        )
        .fetch_all(&self.db)
        .await?;

        println!("original,rating,deviation");

        for p in players.iter() {
            println!("{},{},{}", p.name, p.rating, p.deviation);
        }

        Ok(())
    }

    /// Creates `num` players which all start at rating 2200 / deviation 350.
    ///
    /// The seed determines the player order and — because it also seeds all
    /// randomness used internally by the collection — makes complete runs
    /// reproducible: the same (num, seed, strategy) tuple always plays the
    /// same duels.
    pub async fn new_pre_configured(num: u32, seed: u64) -> Result<ImageCollection> {
        let mut rng = StdRng::seed_from_u64(seed);
        let db_opions = sqlx::sqlite::SqliteConnectOptions::from_str(":memory:")?
            .shared_cache(false)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(3000))
            .locking_mode(sqlx::sqlite::SqliteLockingMode::Exclusive)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true)
            .pragma("temp_store", "MEMORY")
            .pragma("mmap_size", "134217728")
            .pragma("journal_size_limit", "67108864")
            .pragma("cache_size", "64000");

        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(db_opions)
            .await?;

        let db_update_in_progress = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let candidates = std::sync::Arc::new(ArrayQueue::<Duel>::new(20));
        sqlx::query_file!("./schema.sql").execute(&db).await?;

        // generate numbers
        let mut numbers: Vec<u32> = (0..num).collect();
        numbers.shuffle(&mut rng);

        let mut tx = db.begin().await?;
        for i in numbers {
            sqlx::query!(
                "
                INSERT INTO players (name, rating, deviation) 
                VALUES (?, 2200, 350)
                ",
                i
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        Ok(ImageCollection {
            candidates,
            db,
            db_update_in_progress,
            strategy: Strategy::default(),
            rng: std::sync::Arc::new(std::sync::Mutex::new(rng)),
        })
    }

    /// informs the system about the result of a played match
    pub async fn insert_match(&self, m: Match) {
        let db = self.db.clone();
        let can_queue = self.candidates.clone();
        let db_update_in_progress = self.db_update_in_progress.clone();
        let strategy = self.strategy;
        let shared_rng = self.rng.clone();
        tokio::spawn(async move {
            let now = std::time::Instant::now();
            match update_rating(&db, &m).await {
                Err(err) => error!("Error during updating ratings {}", err),
                Ok(_) => info!("Insert update done in {}ms", now.elapsed().as_millis()),
            };

            if can_queue.len() < (can_queue.capacity() / 2)
                && db_update_in_progress
                    .compare_exchange(
                        false,
                        true,
                        std::sync::atomic::Ordering::Acquire,
                        std::sync::atomic::Ordering::Relaxed,
                    )
                    .is_ok()
            {
                info!("refresh duel queue");
                let now = std::time::Instant::now();
                // batch_rng instead of ThreadRng: this future is spawned and must be Send
                let mut rng = batch_rng(&shared_rng);
                if let Ok(new_duels) =
                    calculate_new_matches(&db, can_queue.capacity(), strategy, &mut rng).await
                {
                    // ignore if queue is full
                    for nd in new_duels.into_iter().skip(can_queue.len() + 1) {
                        let _ = can_queue.push(nd); // ignore output
                    }
                }
                info!(
                    "refresh duel queue done in {}ms. Current size: {}",
                    now.elapsed().as_millis(),
                    can_queue.len()
                );
                db_update_in_progress.store(false, std::sync::atomic::Ordering::Release);
            }
        });
    }

    /// switch the matchmaking strategy used for upcoming duels
    pub fn set_strategy(&mut self, strategy: Strategy) {
        self.strategy = strategy;
    }

    /// requests a new duel which needs to be played
    pub async fn new_duel(&self) -> Result<Duel> {
        match self.candidates.pop() {
            Some(duel) => Ok(duel),
            _ => {
                warn!(
                    "No duels in queue. Manually compute one. Try to increase the size of candidate queue."
                );
                let mut rng = batch_rng(&self.rng);
                let duels = calculate_new_matches(&self.db, 3, self.strategy, &mut rng).await?;
                duels
                    .into_iter()
                    .nth(0)
                    .ok_or(anyhow::anyhow!("No candidates found"))
            }
        }
    }
}

async fn check_db_integrity(db: &SqlitePool, image_dir: &String) -> Result<()> {
    let db_files = sqlx::query!("SELECT name FROM players")
        .fetch_all(db)
        .await?;
    let db_files: std::collections::HashSet<String> =
        db_files.into_iter().map(|r| r.name).collect();

    let mut tx = db.begin().await?;

    // check if all files in db exists in fs
    for file in &db_files {
        let file_path = format!("{image_dir}/{file}");
        if !std::path::Path::new(&file_path).is_file() {
            info!("Image path \"{}\" does not exists in ", file);
            sqlx::query!("DELETE FROM players WHERE name = ?", file)
                .execute(&mut *tx)
                .await?;
        }
    }

    for entry in std::fs::read_dir(image_dir)? {
        if let Some(file) = entry?.file_name().to_str()
            && !db_files.contains(file)
        {
            info!("Add \"{}\" to database.", file);
            sqlx::query!(
                "
                    INSERT INTO players (name, rating, deviation) 
                    VALUES (?, 2200, 350)
                    ",
                file
            )
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

/// updates rating based on a played match and also inserts the new match
async fn update_rating(db: &SqlitePool, m: &Match) -> Result<()> {
    let mut tx = db.begin().await?;

    // update the rating
    #[derive(Debug)]
    struct Rating {
        rating: f64,
        deviation: f64,
    }

    let rt_home = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.home_id
    )
    .fetch_one(&mut *tx)
    .await?;
    let rt_guest = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.guest_id
    )
    .fetch_one(&mut *tx)
    .await?;

    let rth = glicko::Rating {
        deviation: rt_home.deviation,
        rating: rt_home.rating,
        time: 0,
    };
    let rtg = glicko::Rating {
        deviation: rt_guest.deviation,
        rating: rt_guest.rating,
        time: 0,
    };
    let won_home = f64::from(m.won);
    let rth_new = glicko::new_rating(&rth, &rtg, won_home, 0, 0_f64);
    let rtg_new = glicko::new_rating(&rtg, &rth, 1.0 - won_home, 0, 0_f64);
    sqlx::query!(
        "UPDATE players SET rating = ?, deviation = ? WHERE id = ?",
        rth_new.rating,
        rth_new.deviation,
        m.home_id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE players SET rating = ?, deviation = ? WHERE id = ?",
        rtg_new.rating,
        rtg_new.deviation,
        m.guest_id
    )
    .execute(&mut *tx)
    .await?;

    // insert the new match
    let result = f64::from(m.won);
    sqlx::query!(
        "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp) VALUES (?, ?, ?, strftime('%Y-%m-%d %H:%M','now'))",
        m.home_id,
        m.guest_id,
        result
    )
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

struct Player {
    id: i64,
    rating: f64,
    deviation: f64,
    name: String,
}

/// pick one candidate at random
fn pick_random(mut candidates: Vec<Player>, rng: &mut impl RngExt) -> Result<Player> {
    if candidates.is_empty() {
        anyhow::bail!("no possible opponent available");
    }
    let idx = rng.random_range(0..candidates.len());
    Ok(candidates.swap_remove(idx))
}

/// the opponents `player_id` played against most recently
async fn recent_opponents(db: &SqlitePool, player_id: i64) -> Result<HashSet<i64>> {
    let rows = sqlx::query!(
        "SELECT home_players_id, guest_players_id FROM matches
        WHERE home_players_id = ? OR guest_players_id = ?
        ORDER BY id DESC
        LIMIT ?",
        player_id,
        player_id,
        RECENT_OPPONENTS
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            if r.home_players_id == player_id {
                r.guest_players_id
            } else {
                r.home_players_id
            }
        })
        .collect())
}

fn retain_eligible(candidates: &mut Vec<Player>, used: &HashSet<i64>, recent: &HashSet<i64>) {
    candidates.retain(|c| !used.contains(&c.id));
    // anti-starvation is a preference: only exclude recent opponents
    // as long as that leaves someone to pick
    if candidates.iter().any(|c| !recent.contains(&c.id)) {
        candidates.retain(|c| !recent.contains(&c.id));
    }
}

/// uniformly random opponent, ignoring ratings
async fn select_uniform_opponent(
    db: &SqlitePool,
    home_id: &Player,
    used: &HashSet<i64>,
    recent: &HashSet<i64>,
    rng: &mut impl RngExt,
) -> Result<Player> {
    let mut candidates = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation, name FROM players
        WHERE id != ?",
        home_id.id
    )
    .fetch_all(db)
    .await?;
    retain_eligible(&mut candidates, used, recent);
    pick_random(candidates, rng)
}

/// always the best performing strategy
///
/// picks a random opponent whose rating lies within
/// `rating ± 0.96 * deviation` of the home player.
/// Falls back to a uniformly random opponent when the window is empty.
async fn select_windowed_opponent(
    db: &SqlitePool,
    home_id: &Player,
    used: &HashSet<i64>,
    recent: &HashSet<i64>,
    rng: &mut impl RngExt,
) -> Result<Player> {
    let upper = home_id.rating + WINDOW_FACTOR * home_id.deviation;
    let lower = home_id.rating - WINDOW_FACTOR * home_id.deviation;

    let mut candidates = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation, name FROM players
        WHERE id != ? AND rating <= ? AND rating >= ?",
        home_id.id,
        upper,
        lower
    )
    .fetch_all(db)
    .await?;
    retain_eligible(&mut candidates, used, recent);

    if candidates.is_empty() {
        select_uniform_opponent(db, home_id, used, recent, rng).await
    } else {
        pick_random(candidates, rng)
    }
}

/// pick an opponent for `home_id` according to `strategy`
async fn select_opponent(
    db: &SqlitePool,
    home_id: &Player,
    strategy: Strategy,
    used: &HashSet<i64>,
    rng: &mut impl RngExt,
) -> Result<Player> {
    let recent = recent_opponents(db, home_id.id).await?;
    match strategy {
        Strategy::Windowed => select_windowed_opponent(db, home_id, used, &recent, rng).await,
        Strategy::Uniform => select_uniform_opponent(db, home_id, used, &recent, rng).await,
    }
}

async fn calculate_new_matches(
    db: &SqlitePool,
    n_matches: usize,
    strategy: Strategy,
    rng: &mut impl RngExt,
) -> Result<Vec<Duel>> {
    let n_matches = n_matches as u32;
    let home_players = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation, name FROM players 
                ORDER BY deviation DESC 
                LIMIT ?",
        n_matches
    )
    .fetch_all(db)
    .await?;

    let mut stream = tokio_stream::iter(home_players);
    let mut result = Vec::new();
    let mut used_guests = HashSet::new();
    while let Some(home_id) = stream.next().await {
        let picked = select_opponent(db, &home_id, strategy, &used_guests, rng).await;
        match picked {
            Ok(guest) => {
                used_guests.insert(guest.id);
                info!("Selected: {} - {}", home_id.name, guest.name);
                result.push(Duel {
                    home: home_id.name,
                    home_id: home_id.id as u32,
                    guest: guest.name,
                    guest_id: guest.id as u32,
                });
            }
            Err(err) => warn!("Skipping {}, no opponent available: {}", home_id.name, err),
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// inserts players with explicit (name, rating, deviation) and returns their ids
    async fn insert_players(db: &SqlitePool, players: &[(&str, f64, f64)]) -> Vec<i64> {
        let mut ids = Vec::with_capacity(players.len());
        for (name, rating, deviation) in players {
            let res = sqlx::query("INSERT INTO players (name, rating, deviation) VALUES (?, ?, ?)")
                .bind(name)
                .bind(*rating)
                .bind(*deviation)
                .execute(db)
                .await
                .unwrap();
            ids.push(res.last_insert_rowid());
        }
        ids
    }

    async fn get_player(db: &SqlitePool, id: i64) -> Player {
        sqlx::query_as!(
            Player,
            "SELECT id, rating, deviation, name FROM players WHERE id = ?",
            id
        )
        .fetch_one(db)
        .await
        .unwrap()
    }

    fn seeded_rng(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    async fn insert_match_fixture(db: &SqlitePool, home: i64, guest: i64) {
        sqlx::query(
            "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp)
            VALUES (?, ?, 1.0, '2000-01-01 00:00')",
        )
        .bind(home)
        .bind(guest)
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_new_pre_configured_creates_correct_number_of_players() {
        let ic = ImageCollection::new_pre_configured(10, 42).await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM players")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert_eq!(count.0, 10);
    }

    #[tokio::test]
    async fn test_new_duel_returns_different_home_and_guest() {
        let ic = ImageCollection::new_pre_configured(5, 42).await.unwrap();
        let duel = ic.new_duel().await.unwrap();
        assert_ne!(duel.home_id, duel.guest_id);
        assert!(!duel.home.is_empty());
        assert!(!duel.guest.is_empty());
    }

    #[tokio::test]
    async fn test_new_duel_fails_with_no_players() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        assert!(ic.new_duel().await.is_err());
    }

    #[tokio::test]
    async fn test_new_duel_fails_with_one_player() {
        let ic = ImageCollection::new_pre_configured(1, 42).await.unwrap();
        assert!(ic.new_duel().await.is_err());
    }

    #[tokio::test]
    async fn test_calculate_new_matches_returns_duels_up_to_n() {
        let ic = ImageCollection::new_pre_configured(5, 42).await.unwrap();
        let mut rng = StdRng::seed_from_u64(1);
        let duels = calculate_new_matches(&ic.db, 3, Strategy::default(), &mut rng)
            .await
            .unwrap();
        assert!(!duels.is_empty());
        assert!(duels.len() <= 3);
        for duel in &duels {
            assert_ne!(duel.home_id, duel.guest_id);
        }
    }

    #[tokio::test]
    async fn test_calculate_new_matches_orders_home_by_deviation() {
        let ic = ImageCollection::new_pre_configured(3, 42).await.unwrap();
        sqlx::query("UPDATE players SET deviation = 100 WHERE id = 1")
            .execute(&ic.db)
            .await
            .unwrap();
        sqlx::query("UPDATE players SET deviation = 200 WHERE id = 2")
            .execute(&ic.db)
            .await
            .unwrap();
        sqlx::query("UPDATE players SET deviation = 300 WHERE id = 3")
            .execute(&ic.db)
            .await
            .unwrap();

        let mut rng = StdRng::seed_from_u64(1);
        let duels = calculate_new_matches(&ic.db, 3, Strategy::default(), &mut rng)
            .await
            .unwrap();
        assert!(!duels.is_empty());
        assert!(duels.len() <= 3);
        // homes must appear in deviation order: id 3 (300), id 2 (200), id 1 (100)
        let order = [3_u32, 2, 1];
        let seen: Vec<usize> = duels
            .iter()
            .map(|d| {
                order
                    .iter()
                    .position(|&id| id == d.home_id)
                    .expect("unexpected home id")
            })
            .collect();
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "homes out of deviation order: {seen:?}"
        );
    }

    #[tokio::test]
    async fn test_select_windowed_stays_inside_rating_window() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 100.0),     // window: [2104, 2296]
                ("inside-a", 2200.0, 100.0), // 2200     in window
                ("inside-b", 2150.0, 50.0),  // 2150     in window
                ("too-low", 1500.0, 100.0),  // 1500     out of window
                ("too-high", 2600.0, 100.0), // 2600     out of window
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(1);
        for _ in 0..50 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Windowed, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            assert_ne!(guest.id, home.id);
            assert!(
                guest.name.starts_with("inside"),
                "guest {} with rating {} escaped the window",
                guest.name,
                guest.rating
            );
        }
    }

    #[tokio::test]
    async fn test_select_windowed_falls_back_when_window_is_empty() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        // deviation 1 shrinks the window to +/- 0.96, so nobody qualifies
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 1.0),
                ("far-below", 1000.0, 1.0),
                ("far-above", 3000.0, 1.0),
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(2);
        for _ in 0..20 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Windowed, &HashSet::new(), &mut rng)
                    .await
                    .expect("empty window must fall back to a uniform pick, not fail");
            assert_ne!(guest.id, home.id);
            assert_ne!(
                guest.rating, 2200.0,
                "fallback should be able to return out-of-window players"
            );
        }
    }

    #[tokio::test]
    async fn test_select_uniform_never_fails_with_two_players() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(&ic.db, &[("a", 2200.0, 350.0), ("b", 2200.0, 350.0)]).await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(3);
        // regression test: the old OFFSET-based sampler lost ~1 in N draws
        // as RowNotFound and silently dropped the duel
        for _ in 0..100 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Uniform, &HashSet::new(), &mut rng)
                    .await
                    .expect("two players must always yield an opponent");
            assert_eq!(guest.id, ids[1]);
        }
    }

    #[tokio::test]
    async fn test_select_opponent_single_player_is_an_error_not_a_panic() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(&ic.db, &[("only", 2200.0, 350.0)]).await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(4);
        for strategy in [Strategy::Windowed, Strategy::Uniform] {
            assert!(
                select_opponent(&ic.db, &home, strategy, &HashSet::new(), &mut rng)
                    .await
                    .is_err(),
                "strategy {strategy:?} must report an error when no opponent exists"
            );
        }
    }

    #[tokio::test]
    async fn test_select_uniform_covers_every_candidate() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("cand-1", 2200.0, 350.0),
                ("cand-2", 2200.0, 350.0),
                ("cand-3", 2200.0, 350.0),
                ("cand-4", 2200.0, 350.0),
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut counts = std::collections::HashMap::new();
        let mut rng = seeded_rng(5);
        for _ in 0..1000 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Uniform, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            assert_ne!(guest.id, home.id, "home player must never be picked");
            *counts.entry(guest.id).or_insert(0usize) += 1;
        }
        for cand in &ids[1..] {
            assert!(
                counts.contains_key(cand),
                "candidate {cand} was never selected — starvation"
            );
        }
        assert_eq!(counts.len(), 4);
    }

    #[tokio::test]
    async fn test_select_uniform_is_roughly_balanced() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("cand-1", 2200.0, 350.0),
                ("cand-2", 2200.0, 350.0),
                ("cand-3", 2200.0, 350.0),
                ("cand-4", 2200.0, 350.0),
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut counts = std::collections::HashMap::new();
        let mut rng = seeded_rng(6);
        for _ in 0..4000 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Uniform, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            *counts.entry(guest.id).or_insert(0usize) += 1;
        }
        // 4000 draws over 4 candidates => 1000 each; generous +/- 25% band
        for cand in &ids[1..] {
            let count = counts.get(cand).copied().unwrap_or(0);
            assert!(
                (750..=1250).contains(&count),
                "candidate {cand} selected {count} times, expected roughly 1000"
            );
        }
    }

    #[tokio::test]
    async fn test_select_uniform_ignores_the_rating_window() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 100.0), // window: [2104, 2296]
                ("far-away", 1500.0, 100.0),
                ("inside", 2200.0, 100.0),
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(7);
        let mut saw_out_of_window = false;
        for _ in 0..50 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Uniform, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            if guest.id == ids[1] {
                saw_out_of_window = true;
            }
        }
        assert!(
            saw_out_of_window,
            "uniform strategy must not be limited to the rating window"
        );
    }

    #[tokio::test]
    async fn test_select_opponent_respects_used_guests() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("cand-a", 2200.0, 350.0),
                ("cand-b", 2200.0, 350.0),
            ],
        )
        .await;

        let home = get_player(&ic.db, ids[0]).await;
        let used: HashSet<i64> = [ids[1]].into_iter().collect();
        let mut rng = seeded_rng(12);
        for _ in 0..50 {
            let guest = select_opponent(&ic.db, &home, Strategy::Uniform, &used, &mut rng)
                .await
                .unwrap();
            assert_eq!(guest.id, ids[2], "used guests must never be returned");
        }

        let used: HashSet<i64> = [ids[1], ids[2]].into_iter().collect();
        assert!(
            select_opponent(&ic.db, &home, Strategy::Uniform, &used, &mut rng)
                .await
                .is_err(),
            "once every candidate is used up, selection must fail"
        );
    }

    #[tokio::test]
    async fn test_select_excludes_recent_opponents() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("recent-a", 2200.0, 350.0),
                ("fresh-b", 2200.0, 350.0),
                ("recent-d", 2200.0, 350.0),
                ("fresh-c", 2200.0, 350.0),
            ],
        )
        .await;

        insert_match_fixture(&ic.db, ids[0], ids[1]).await;
        insert_match_fixture(&ic.db, ids[3], ids[0]).await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(13);
        let mut picked = HashSet::new();
        for _ in 0..50 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Windowed, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            assert_ne!(guest.id, ids[0]);
            assert!(
                guest.id != ids[1] && guest.id != ids[3],
                "picked recent opponent {}",
                guest.name
            );
            picked.insert(guest.name);
        }
        assert!(
            picked.contains("fresh-b") && picked.contains("fresh-c"),
            "both never-played opponents must stay reachable: {picked:?}"
        );
    }

    #[tokio::test]
    async fn test_select_degrades_when_everyone_is_recent() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("old-a", 2200.0, 350.0),
                ("old-b", 2200.0, 350.0),
            ],
        )
        .await;

        insert_match_fixture(&ic.db, ids[0], ids[1]).await;
        insert_match_fixture(&ic.db, ids[0], ids[2]).await;

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(14);
        for _ in 0..20 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Windowed, &HashSet::new(), &mut rng)
                    .await
                    .expect("anti-starvation must not starve the batch when everyone is recent");
            assert!(guest.id == ids[1] || guest.id == ids[2]);
        }
    }

    #[tokio::test]
    async fn test_recent_opponents_expire_after_n() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        let ids = insert_players(
            &ic.db,
            &[
                ("home", 2200.0, 350.0),
                ("oldest", 2200.0, 350.0),
                ("mid-b", 2200.0, 350.0),
                ("mid-c", 2200.0, 350.0),
                ("newest", 2200.0, 350.0),
            ],
        )
        .await;

        for id in &ids[1..] {
            insert_match_fixture(&ic.db, ids[0], *id).await;
        }

        let home = get_player(&ic.db, ids[0]).await;
        let mut rng = seeded_rng(15);
        for _ in 0..50 {
            let guest =
                select_opponent(&ic.db, &home, Strategy::Windowed, &HashSet::new(), &mut rng)
                    .await
                    .unwrap();
            assert_eq!(
                guest.id, ids[1],
                "only the opponent beyond the last {RECENT_OPPONENTS} must remain eligible"
            );
        }
    }

    #[test]
    fn test_strategy_from_str() {
        assert_eq!("windowed".parse::<Strategy>().unwrap(), Strategy::Windowed);
        assert_eq!("Windowed".parse::<Strategy>().unwrap(), Strategy::Windowed);
        assert_eq!("uniform".parse::<Strategy>().unwrap(), Strategy::Uniform);
        assert!("nope".parse::<Strategy>().is_err());
    }

    #[tokio::test]
    async fn test_calculate_new_matches_never_reuses_guests() {
        let ic = ImageCollection::new_pre_configured(0, 42).await.unwrap();
        insert_players(
            &ic.db,
            &[
                ("p1", 2200.0, 350.0),
                ("p2", 2200.0, 350.0),
                ("p3", 2200.0, 350.0),
                ("p4", 2200.0, 350.0),
                ("p5", 2200.0, 350.0),
                ("p6", 2200.0, 350.0),
            ],
        )
        .await;

        let mut rng = seeded_rng(8);
        let duels = calculate_new_matches(&ic.db, 6, Strategy::Uniform, &mut rng)
            .await
            .unwrap();
        let guests: std::collections::HashSet<u32> = duels.iter().map(|d| d.guest_id).collect();
        assert_eq!(
            guests.len(),
            duels.len(),
            "guest ids must be unique within one batch"
        );
    }

    #[tokio::test]
    async fn test_update_rating_increases_winner_rating() {
        let ic = ImageCollection::new_pre_configured(2, 42).await.unwrap();
        let m = Match {
            home_id: 1,
            guest_id: 2,
            won: MatchOutcome::HomeWin,
        };
        update_rating(&ic.db, &m).await.unwrap();

        let home: (f64, f64) = sqlx::query_as("SELECT rating, deviation FROM players WHERE id = 1")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert!(home.0 > 2200.0);
        assert!(home.1 < 350.0);

        let guest: (f64, f64) =
            sqlx::query_as("SELECT rating, deviation FROM players WHERE id = 2")
                .fetch_one(&ic.db)
                .await
                .unwrap();
        assert!(guest.0 < 2200.0);
        assert!(guest.1 < 350.0);

        let match_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM matches")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert_eq!(match_count.0, 1);
    }

    #[tokio::test]
    async fn test_draw_does_not_change_ratings_significantly() {
        let ic = ImageCollection::new_pre_configured(2, 42).await.unwrap();
        let m = Match {
            home_id: 1,
            guest_id: 2,
            won: MatchOutcome::Draw,
        };
        update_rating(&ic.db, &m).await.unwrap();

        let home: (f64, f64) = sqlx::query_as("SELECT rating, deviation FROM players WHERE id = 1")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert!((home.0 - 2200.0).abs() < 50.0);
        assert!(home.1 < 350.0);

        let match_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM matches")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert_eq!(match_count.0, 1);
    }

    #[test]
    fn test_match_outcome_serde_roundtrip() {
        assert_eq!(
            serde_json::from_str::<MatchOutcome>("1.0").unwrap(),
            MatchOutcome::HomeWin
        );
        assert_eq!(
            serde_json::from_str::<MatchOutcome>("0.5").unwrap(),
            MatchOutcome::Draw
        );
        assert_eq!(
            serde_json::from_str::<MatchOutcome>("0.0").unwrap(),
            MatchOutcome::GuestWin
        );
        assert!(serde_json::from_str::<MatchOutcome>("2.0").is_err());
        assert!(serde_json::from_str::<MatchOutcome>("-1.0").is_err());
        assert_eq!(
            serde_json::to_string(&MatchOutcome::HomeWin).unwrap(),
            "1.0"
        );
        assert_eq!(serde_json::to_string(&MatchOutcome::Draw).unwrap(), "0.5");
        assert_eq!(
            serde_json::to_string(&MatchOutcome::GuestWin).unwrap(),
            "0.0"
        );
    }
}
