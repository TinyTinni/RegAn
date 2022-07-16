use anyhow::Result;
use futures::StreamExt;
use clap::Parser;
use image_collection::{ImageCollection, Match};

#[derive(Parser, Debug)]
#[clap(about, version, author)]
struct Args {
    /// The output database. Must be a file as only sqlite is currently supported
    /// Write the path in the form of "sqlite://<rel-path>"
    #[clap(short, long)]
    samples: usize,

    /// The queue buffer. Candidates gets pre-computed.
    /// The precomputation lowers the precision of the matchmaking
    /// but also reduces the possible latency of the next match up
    #[clap(short, long)]
    games: usize,

    //#[clap(long, default_value_t = 0.0_f64)]
    //uncertinty: f64,

    /// Prints timings for program runtime
    #[clap(long, default_value_t = false, value_parser)]
    print_timing: bool,

    /// Omit printing resulting CSV 
    #[clap(long, default_value_t = false, value_parser)]
    no_csv: bool,
}

#[actix_web::main]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Warn)
        .init();

    let args = Args::parse();

    // configure db
    let collection = ImageCollection::new_pre_configured(args.samples as u32).await?;

    let stream = futures::stream::iter(0..args.games);
    let start = std::time::Instant::now();

    stream.for_each_concurrent(1, |_| async  {
        let new_duel = collection.new_duel().await.unwrap();
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
        collection.insert_match(&m).await.unwrap();
    }).await;

    if !args.no_csv
    {
        collection.print_csv().await?;
    }

    if args.print_timing
    {
        let runs_per_sec = args.games as f64 / start.elapsed().as_secs_f64();
        println!("runs per sec: {}", runs_per_sec);
    }

    Ok(())
}
