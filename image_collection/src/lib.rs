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

    pub async fn to_csv(&self) -> Result<()> {
        struct Player {
            name: String,
            rating: f32,
            deviation: f32,
        };

        let players = sqlx::query_as!(Player, "SELECT name, rating, deviation FROM players ORDER BY rating")
            .fetch_all(&self.db)
            .await?;

        for p in players {
            println! {"{} rat {} dev {}", p.name, p.rating, p.deviation};
        }

        Ok(())
    }

    pub async fn new_pre_configured(num: u32) -> Result<ImageCollection> {
        let db_opions = sqlx::sqlite::SqliteConnectOptions::from_str(":memory:")?
            .shared_cache(true)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(3000));
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(db_opions)
            .await?;
        sqlx::query("PRAGMA busy_timeout = 3000")
            .execute(&db)
            .await?;

        let db_update_in_progress = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let candidates = std::sync::Arc::new(ArrayQueue::<Duel>::new(20));
        sqlx::query_file!("./schema.sql").execute(&db).await?;

        let mut tx = db.begin().await?;
        for i in 0..num {
            sqlx::query!(
                "
                INSERT INTO players (name, rating, deviation) 
                VALUES (?, 2200, 350)
                ",
                i
            )
            .execute(&mut tx)
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
    pub async fn insert_match(&self, m: &Match) -> Result<()> {
        let db = self.db.clone();
        let can_queue = self.candidates.clone();
        let m = m.clone();
        let db_update_in_progress = self.db_update_in_progress.clone();

        tokio::spawn(async move {
            let now = std::time::Instant::now();
            match update_rating(&db, &m).await {
                Err(err) => error!("Error during updating ratings {}", err),
                Ok(_) => info!(
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
                let duels = calculate_new_matches(&self.db, 5).await?;
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
        // matchmaking
        // the current idea is to select one with the current constraints:
        // - should be in the range of the players "limit"
        // - should also be a player which has not played often
        // hopium says that this would lessen the requirements of #matches
        // for all the players and that the deviation reduction
        // is more uniformely distributred.
        // But also, that, when the deviation is low, the deviations
        // do not differ too much to have a better resolution of the ranking
        // todo: make some smulations of different matchmakings?
        let upper = home_id.rating + 1.96 * home_id.deviation;
        let lower = home_id.rating - 1.96 * home_id.deviation;
        let limit = 2 * n_matches;
        match sqlx::query_as!(
            Player,
            "SELECT id, rating, deviation, name FROM players 
            WHERE id IN (SELECT id FROM players 
                WHERE id != $1 AND
                rating <= $2 AND
                rating >= $3
                ORDER BY deviation DESC
                LIMIT $4)",
            home_id.id,
            upper,
            lower,
            limit
        )
        .fetch_all(db)
        .await
        {
            // search for candidates which are not already in the buffer as guests
            // players in the queue have already an updated deviation and should
            // not occur here, unless their deviation is already high.
            // If their deviation is high, they should get selected multiple times
            // in order to lower their deviation fast
            Ok(mut guest_ids) => {
                if guest_ids.len() == 0 {
                    let guest_id = sqlx::query_as!(Player, "SELECT id, rating, deviation, name FROM players
                    WHERE id != $1 ORDER BY RANDOM() LIMIT 1", home_id.id).fetch_one(db).await?;
                    result.push(Duel {
                        home: home_id.name,
                        home_id: home_id.id as u32,
                        guest: guest_id.name,
                        guest_id: guest_id.id as u32,
                    })
                } else {
                    guest_ids.retain(|x| {
                        !result
                            .iter()
                            .map(|y: &Duel| i64::from(y.guest_id))
                            .any(|y| y == x.id)
                    });
                    // if found, insert them
                    match guest_ids.into_iter().next() {
                        Some(guest_id) => result.push(Duel {
                            home: home_id.name,
                            home_id: home_id.id as u32,
                            guest: guest_id.name,
                            guest_id: guest_id.id as u32,
                        }),
                        _ => {}
                    }
                }
            }
            _ => {}
        };
    }

    Ok(result)
}
