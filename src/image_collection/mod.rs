extern crate crossbeam;
extern crate rand_distr;
extern crate sqlx;
extern crate tokio;
extern crate tokio_stream;

mod glicko;

use anyhow::Result;
use crossbeam::queue::ArrayQueue;
use rand_distr::{Distribution, Normal};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::str::FromStr;
use tokio_stream::StreamExt;

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
    pub async fn new(options: &ImageCollectionOptions) -> Result<ImageCollection> {
        let db_options =
            sqlx::sqlite::SqliteConnectOptions::from_str(&options.db_path)?.create_if_missing(true);

        let db = SqlitePool::connect_with(db_options).await?;
        sqlx::query_file!("./schema.sql").execute(&db).await?;
        check_db_integrity(&db).await?;
        let candidates = std::sync::Arc::new(ArrayQueue::<Duel>::new(options.candidate_buffer));
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

    /// informs the system about the result of a played match
    pub async fn insert_match(&self, m: &Match) -> Result<()> {
        let db = self.db.clone();
        let can_queue = self.candidates.clone();
        let m = m.clone();
        let db_update_in_progress = self.db_update_in_progress.clone();

        tokio::spawn(async move {
            let now = std::time::Instant::now();
            match update_rating(&db, &m).await {
                Err(err) => error!("Error during updating ratings {}", err),
                Ok(_) => println!(
                    "Insert update done in {} microseconds",
                    now.elapsed().as_micros()
                ),
            };

            if can_queue.len() < (can_queue.capacity() / 2) {
                if db_update_in_progress
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
                    match calculate_new_matches(&db, can_queue.capacity()).await {
                        Ok(new_duels) => {
                            // ignore if queue is full
                            for nd in new_duels.into_iter().skip(can_queue.len() + 1) {
                                let _ = can_queue.push(nd); // ignore output
                            }
                        }
                        _ => {}
                    }
                    info!(
                        "refresh duel queue done in {} microseconds. Current size: {}",
                        now.elapsed().as_micros(),
                        can_queue.len()
                    );
                    db_update_in_progress.store(false, std::sync::atomic::Ordering::Release);
                }
            }
        });
        Ok(())
    }

    /// requests a new duel which needs to be played
    pub async fn new_duel(&self) -> Result<Duel> {
        match self.candidates.pop() {
            Some(duel) => return Ok(duel),
            None => {
                warn!("No duels in queue. Manually compute one. Try to increase the size of candidate queue.");
                //todo: use a cell on candidate_queue to increase its capacity
                // calculate new matches full on it
                let duels = calculate_new_matches(&self.db, 1).await?;
                if !duels.is_empty() {
                    return Ok(duels.into_iter().nth(0).unwrap());
                } else {
                    return Err(anyhow::Error::msg("No Candidates found.".to_owned()));
                    //TODO try to compute new ones
                }
            }
        }
    }
}

async fn check_db_integrity(db: &SqlitePool) -> Result<()> {
    let path = "images";
    let db_files = sqlx::query!("SELECT name FROM players")
        .fetch_all(db)
        .await?;
    let db_files: std::collections::HashSet<String> =
        db_files.into_iter().map(|r| r.name).collect();

    let mut tx = db.begin().await?;

    // check if all files in db exists in fs
    for file in &db_files {
        let file_path = format!("{}/{}", path, file);
        if !std::path::Path::new(&file_path).is_file() {
            info!("Image path \"{}\" does not exists in ", file);
            sqlx::query!("DELETE FROM players WHERE name = ?", file)
                .execute(&mut tx)
                .await?;
        }
    }

    for entry in std::fs::read_dir(path)? {
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
                .execute(&mut tx)
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
    struct Rating {
        rating: f32,
        deviation: f32,
    }

    let rt_home = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.home_id
    )
    .fetch_one(&mut tx)
    .await?;
    let rt_guest = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.guest_id
    )
    .fetch_one(&mut tx)
    .await?;

    let rth = glicko::Rating {
        deviation: rt_home.deviation as f64,
        rating: rt_home.rating as f64,
        time: 0,
    };
    let rtg = glicko::Rating {
        deviation: rt_guest.deviation as f64,
        rating: rt_guest.rating as f64,
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
    .execute(&mut tx)
    .await?;
    sqlx::query!(
        "UPDATE players SET rating = ?, deviation = ? WHERE id = ?",
        rtg_new.rating,
        rtg_new.deviation,
        m.guest_id
    )
    .execute(&mut tx)
    .await?;

    // insert the new match
    sqlx::query!(
        "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp) VALUES (?, ?, ?, strftime('%Y-%m-%d %H %M','now'))",
        m.home_id,
        m.guest_id,
        m.won
    )
    .execute(&mut tx)
    .await?;

    tx.commit().await?;

    Ok(())
}

async fn calculate_new_matches(db: &SqlitePool, n_matches: usize) -> Result<Vec<Duel>> {
    struct Player {
        id: i64,
        rating: f32,
        deviation: f32,
        name: String,
    }
    let n_matches = n_matches as u32;
    let home_ids = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation AS deviation, name FROM players WHERE 
             id IN 
               (SELECT id FROM players 
                ORDER BY deviation DESC 
                LIMIT ?)",
        n_matches
    )
    .fetch_all(db)
    .await?;

    let mut stream = tokio_stream::iter(home_ids);
    let mut result = Vec::new();
    while let Some(home_id) = stream.next().await {
        // randomly select a candidate from a normal distribution
        let normal_distr = Normal::new(home_id.rating, home_id.deviation).unwrap();
        let next_nearest_rating = normal_distr.sample(&mut rand::thread_rng()).abs();

        // select a candidate nearest to the randomly selected rating
        match sqlx::query_as!(
            Player,
            "SELECT id, rating, deviation, name FROM players 
            WHERE id IN (SELECT id FROM players 
                WHERE id != $1 
                ORDER BY ABS(rating - $2) ASC 
                LIMIT 1)",
            home_id.id,
            next_nearest_rating
        )
        .fetch_one(db)
        .await
        {
            Ok(guest_id) => result.push(Duel {
                home: home_id.name,
                home_id: home_id.id as u32,
                guest: guest_id.name,
                guest_id: guest_id.id as u32,
            }),
            _ => {}
        };
    }

    Ok(result)
}
