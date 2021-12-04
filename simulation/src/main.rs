#[macro_use]
extern crate sqlx;
use anyhow::Result;
use image_collection::{ImageCollection, ImageCollectionOptions, Match};

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Info)
        .init();

    // configure db
    let collection = ImageCollection::new_pre_configured(500).await?;

    let runs = 10_000;

    for _i in 0..runs {
        let new_duel = collection.new_duel().await?;
        let home_id = new_duel.home_id;
        let guest_id = new_duel.guest_id;
        let won = {
            if home_id > guest_id {
                1_f32
            } else {
                0_f32
            }
        };
        let m = Match {
            home_id,
            guest_id,
            won,
        };
        collection.insert_match(&m).await?;
    }

    collection.to_csv().await?;

    Ok(())
}
