#[macro_use]
extern crate log;
extern crate crossbeam;
extern crate sqlx;
extern crate tokio;
extern crate tokio_stream;

mod glicko;

use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::str::FromStr;
use tokio_stream::StreamExt;

use rand::prelude::*;

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

/// a played match with the given result
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Match {
    pub home_id: u32,
    pub guest_id: u32,
    /// 0 if home lost, 0.5 on draw, 1 if home won
    /// todo: make an enum out of it
    pub won: f32,
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
            .pragma("cache_size", "2000")
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
            } else {
                let new_buffer = std::cmp::min(3, max_players);
                warn!("Max players exceeds candidate buffer. Lowering candidate buffer to {}. Player count: {}",new_buffer, max_players);
                new_buffer
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
            .pragma("cache_size", "2000");

        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(db_opions)
            .await?;

        let db_update_in_progress = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let candidates = std::sync::Arc::new(ArrayQueue::<Duel>::new(20));
        sqlx::query_file!("./schema.sql").execute(&db).await?;

        // generate numbers
        let mut numbers: Vec<u32> = (0..num).collect();
        let mut rng = rand::thread_rng();
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
                    "refresh duel queue done in {} microseconds. Current size: {}",
                    now.elapsed().as_millis(),
                    can_queue.len()
                );
                db_update_in_progress.store(false, std::sync::atomic::Ordering::Release);
            }
        });
    }

    /// requests a new duel which needs to be played
    pub async fn new_duel(&self) -> Result<Duel> {
        if let Some(duel) = self.candidates.pop() {
            Ok(duel)
        } else {
            warn!("No duels in queue. Manually compute one. Try to increase the size of candidate queue.");
            let duels = calculate_new_matches(&self.db, 3).await?;
            duels
                .into_iter()
                .nth(0)
                .ok_or(anyhow::anyhow!("No candidates found"))
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
        let file_path = format!("{}/{}", image_dir, file);
        if !std::path::Path::new(&file_path).is_file() {
            info!("Image path \"{}\" does not exists in ", file);
            sqlx::query!("DELETE FROM players WHERE name = ?", file)
                .execute(&mut *tx)
                .await?;
        }
    }

    for entry in std::fs::read_dir(image_dir)? {
        if let Some(file) = entry?.file_name().to_str() {
            if !db_files.contains(file) {
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
    let rth_new = glicko::new_rating(&rth, &rtg, m.won as f64, 0, 0_f64);
    let rtg_new = glicko::new_rating(&rtg, &rth, 1_f64 - m.won as f64, 0, 0_f64);
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
    sqlx::query!(
        "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp) VALUES (?, ?, ?, strftime('%Y-%m-%d %H %M','now'))",
        m.home_id,
        m.guest_id,
        m.won
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
        let mut rng = rand::thread_rng();
        let distr = rand::distributions::Uniform::new(0, all - 1);
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
        info!("Selected: {}", home_id.name);
        if let Ok(guest) = select_random_player(db, &home_id).await {
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
