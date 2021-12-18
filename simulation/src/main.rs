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
       let start = std::time::Instant::now();
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

    let runs_per_sec = runs as f64 / start.elapsed().as_secs_f64();
    collection.to_csv().await?;
    
    println!("runs per sec: {}", runs_per_sec);
    let sqre = 0;
    
    

    Ok(())
}
