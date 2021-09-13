mod ranking;

use anyhow::Result;
use rand::prelude::IteratorRandom;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

// pub struct Collection{

// }

#[derive(Serialize, Deserialize)]
pub struct Duel {
    pub home: String,
    pub home_id: u32,
    pub guest: String,
    pub guest_id: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Match {
    pub home_id: u32,
    pub guest_id: u32,
    pub won: f32,
}

pub async fn check_db_integrity(db: &SqlitePool) -> Result<()> {
    let path = "images";
    let db_files = sqlx::query!("SELECT name FROM players")
        .fetch_all(db)
        .await?;
    let db_files: std::collections::HashSet<String> =
        db_files.into_iter().map(|r| r.name).collect();

    // check if all files in db exists in fs
    for file in &db_files {
        let file_path = format!("{}/{}", path, file);
        if !std::path::Path::new(&file_path).is_file() {
            info!("Image path \"{}\" does not exists in ", file);
            sqlx::query!("DELETE FROM players WHERE name = ?", file)
                .execute(db)
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
                .execute(db)
                .await?;
            }
        }
    }
    Ok(())
}

pub async fn update_rating(db: &SqlitePool, m: &Match) -> Result<()> {
    struct Rating {
        rating: f32,
        deviation: f32,
    }

    // todo: mutex around db access

    let rt_home = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.home_id
    )
    .fetch_one(db)
    .await?;
    let rt_guest = sqlx::query_as!(
        Rating,
        "SELECT rating, deviation FROM players WHERE id = ?",
        m.guest_id
    )
    .fetch_one(db)
    .await?;

    let rth = ranking::Rating {
        deviation: rt_home.deviation as f64,
        rating: rt_home.rating as f64,
        time: 0,
    };
    let rtg = ranking::Rating {
        deviation: rt_guest.deviation as f64,
        rating: rt_guest.rating as f64,
        time: 0,
    };
    let rth_new = ranking::new_rating(&rth, &rtg, m.won as f64, 0, 0_f64);
    let rtg_new = ranking::new_rating(&rtg, &rth, 1_f64 - m.won as f64, 0, 0_f64);
    sqlx::query!(
        "UPDATE players SET rating = ?, deviation = ? WHERE id = ?",
        rth_new.rating,
        rth_new.deviation,
        m.home_id
    )
    .execute(db)
    .await?;
    sqlx::query!(
        "UPDATE players SET rating = ?, deviation = ? WHERE id = ?",
        rtg_new.rating,
        rtg_new.deviation,
        m.guest_id
    )
    .execute(db)
    .await?;

    Ok(())
}

pub async fn insert_match(db: &SqlitePool, m: &Match) -> Result<()> {
    sqlx::query!(
        "INSERT INTO matches (home_players_id, guest_players_id, result, timestamp) VALUES (?, ?, ?, strftime('%Y-%m-%d %H %M','now'))",
        m.home_id,
        m.guest_id,
        m.won
    )
    .execute(db)
    .await?;
    Ok(())
}

// fn random_file() -> Result<String> {
//     let dir = std::fs::read_dir("./test_images").expect("Could not find image directory.");
//     let mut rng = rand::thread_rng();
//     //todo: get rid of unwrap
//     let fname = dir
//         .choose(&mut rng)
//         .unwrap()?
//         .file_name()
//         .into_string()
//         .unwrap();
//     Ok(fname)
// }

pub async fn calculate_new_match(db: &SqlitePool) -> Result<Duel> {
    calculate_new_match2(&db).await
    // let home = random_file()?;
    // let guest = random_file()?;
    // struct Id {
    //     id: i64,
    // }
    // let home_id = sqlx::query_as!(Id, "SELECT id FROM players WHERE name = ?", home)
    //     .fetch_one(db)
    //     .await?;
    // let guest_id = sqlx::query_as!(Id, "SELECT id FROM players WHERE name = ?", guest)
    //     .fetch_one(db)
    //     .await?;
    // Ok(Duel {
    //     home,
    //     home_id: home_id.id as u32,
    //     guest,
    //     guest_id: guest_id.id as u32,
    // })
}

async fn calculate_new_match2(db: &SqlitePool) -> Result<Duel> {
    struct Player {
        id: i64,
        rating: f32,
        deviation: f32,
        name: String,
    }
    let home_id = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation AS deviation, name FROM players WHERE 
             id IN 
               (SELECT id FROM players 
                ORDER BY deviation DESC 
                LIMIT 20)"
    )
    .fetch_all(db)
    .await?;

    let home_id = match home_id.into_iter().choose(&mut rand::thread_rng()) {
        Some(it) => Ok(it),
        None => Err(anyhow::Error::msg(
            "No Image Element found. Image Database seems to be empty.",
        )),
    }?;

    // todo: random pick normal distributed around home_id.rating instead of unknown distribution
    let guest_id = sqlx::query_as!(
        Player,
        "SELECT id, rating, deviation, name FROM players 
            WHERE id IN (SELECT id FROM players 
                WHERE id != $1 AND
                rating <= $2 + 1.96 * $3 AND
                rating >= $2 - 1.96 * $3
                ORDER BY RANDOM() 
                LIMIT 1)",
        home_id.id,
        home_id.rating,
        home_id.deviation
    )
    .fetch_one(db)
    .await?;

    Ok(Duel {
        home: home_id.name,
        home_id: home_id.id as u32,
        guest: guest_id.name,
        guest_id: guest_id.id as u32,
    })
}
