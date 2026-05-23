#[macro_use]
mod glicko;

use anyhow::Result;
use crossbeam_queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
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

    db: SqlitePool,
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
        let new_duels = calculate_new_matches(&db, candidates.capacity()).await?;
        for nd in new_duels.into_iter() {
            let _ = candidates.push(nd);
        }
        let db_update_in_progress = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        Ok(ImageCollection {
            candidates,
            db,
            db_update_in_progress,
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

        println! {"original,rating,deviation"};

        for p in players.iter() {
            println! {"{},{},{}", &p.name, &p.rating, &p.deviation};
        }

        Ok(())
    }

    pub async fn new_pre_configured(num: u32) -> Result<ImageCollection> {
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
        let mut rng = rand::rng();
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
        })
    }

    /// informs the system about the result of a played match
    pub async fn insert_match(&self, m: Match) {
        let db = self.db.clone();
        let can_queue = self.candidates.clone();
        let db_update_in_progress = self.db_update_in_progress.clone();
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
                if let Ok(new_duels) = calculate_new_matches(&db, can_queue.capacity()).await {
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

    /// requests a new duel which needs to be played
    pub async fn new_duel(&self) -> Result<Duel> {
        match self.candidates.pop() {
            Some(duel) => Ok(duel),
            _ => {
                warn!(
                    "No duels in queue. Manually compute one. Try to increase the size of candidate queue."
                );
                let duels = calculate_new_matches(&self.db, 3).await?;
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
        "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp) VALUES (?, ?, ?, strftime('%Y-%m-%d %H %M','now'))",
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

async fn select_random_player_uniform(db: &SqlitePool, home_id: &Player) -> Result<Player> {
    let all = sqlx::query_scalar!("SELECT COUNT(*) FROM players")
        .fetch_one(db)
        .await?;
    let rnd = {
        let mut rng = rand::rng();
        let distr = rand::distr::Uniform::new(0, all - 1)?;
        rng.sample(distr)
    };
    let random_id = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation, name FROM players
        WHERE id != $1 LIMIT 1 OFFSET $2",
        home_id.id,
        rnd
    )
    .fetch_one(db)
    .await?;
    Ok(random_id)
}

/// always the best performing strategy
async fn select_random_player(db: &SqlitePool, home_id: &Player) -> Result<Player> {
    // matchmaking
    // todo: make some smulations of different matchmakings?
    let upper = home_id.rating + 0.96 * home_id.deviation;
    let lower = home_id.rating - 0.96 * home_id.deviation;

    let random_id = sqlx::query_as!(
        Player,
        // "SELECT id, rating, deviation, name FROM players
        // WHERE id IN (SELECT id FROM players
        //    WHERE id != $1 AND
        //    rating <= $2 AND
        //    rating >= $3
        //    ORDER BY RANDOM() LIMIT 1)",
        "SELECT id, rating, deviation, name FROM players
            WHERE id IN (SELECT id FROM players
                WHERE id != $1 AND
                rating <= $2 AND
                rating >= $3
                LIMIT 1 OFFSET (ABS(RANDOM()) % (SELECT COUNT(*)
                    FROM players WHERE id != $1 AND
                    rating <= $2 AND
                    rating >= $3))
                )",
        home_id.id,
        upper,
        lower
    )
    .fetch_one(db)
    .await;

    match random_id {
        Ok(r) => Ok(r),
        _ => select_random_player_uniform(db, home_id).await,
    }
}

async fn calculate_new_matches(db: &SqlitePool, n_matches: usize) -> Result<Vec<Duel>> {
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
    while let Some(home_id) = stream.next().await {
        if let Ok(guest) = select_random_player(db, &home_id).await {
            info!("Selected: {} - {}", home_id.name, guest.name);
            result.push(Duel {
                home: home_id.name,
                home_id: home_id.id as u32,
                guest: guest.name,
                guest_id: guest.id as u32,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_new_pre_configured_creates_correct_number_of_players() {
        let ic = ImageCollection::new_pre_configured(10).await.unwrap();
        let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM players")
            .fetch_one(&ic.db)
            .await
            .unwrap();
        assert_eq!(count.0, 10);
    }

    #[tokio::test]
    async fn test_new_duel_returns_different_home_and_guest() {
        let ic = ImageCollection::new_pre_configured(5).await.unwrap();
        let duel = ic.new_duel().await.unwrap();
        assert_ne!(duel.home_id, duel.guest_id);
        assert!(!duel.home.is_empty());
        assert!(!duel.guest.is_empty());
    }

    #[tokio::test]
    async fn test_new_duel_fails_with_no_players() {
        let ic = ImageCollection::new_pre_configured(0).await.unwrap();
        assert!(ic.new_duel().await.is_err());
    }

    #[tokio::test]
    async fn test_new_duel_fails_with_one_player() {
        let ic = ImageCollection::new_pre_configured(1).await.unwrap();
        assert!(ic.new_duel().await.is_err());
    }

    #[tokio::test]
    async fn test_calculate_new_matches_returns_duels_up_to_n() {
        let ic = ImageCollection::new_pre_configured(5).await.unwrap();
        let duels = calculate_new_matches(&ic.db, 3).await.unwrap();
        assert!(!duels.is_empty());
        assert!(duels.len() <= 3);
        for duel in &duels {
            assert_ne!(duel.home_id, duel.guest_id);
        }
    }

    #[tokio::test]
    async fn test_calculate_new_matches_orders_home_by_deviation() {
        let ic = ImageCollection::new_pre_configured(3).await.unwrap();
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

        let duels = calculate_new_matches(&ic.db, 3).await.unwrap();
        assert_eq!(duels.len(), 3);
        assert_eq!(duels[0].home_id, 3);
        assert_eq!(duels[1].home_id, 2);
        assert_eq!(duels[2].home_id, 1);
    }

    #[tokio::test]
    async fn test_update_rating_increases_winner_rating() {
        let ic = ImageCollection::new_pre_configured(2).await.unwrap();
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
        let ic = ImageCollection::new_pre_configured(2).await.unwrap();
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
