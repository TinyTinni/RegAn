use anyhow::Result;
use futures::StreamExt;
use clap::Parser;
use image_collection::{ImageCollection, Match};
use rand_distr::{Normal, Distribution};

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

    /// standard deviation for uncertainty.
    /// When you got this task, a human can usually not clearly distinct if something is better or worse
    /// There might be some contradiction if the assessment of a pair, so that i.e. 10 < 9
    /// The standard deviation tries to model this by skeweing one value by a random, normal distributed value
    /// Set to 0 if you want to disable this in the simulation
    #[clap(long, default_value_t = 0.0_f64)]
    std_dev: f64,

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

    let distribution = Normal::new(0.0 as f64, args.std_dev)?;

    stream.for_each_concurrent(1, |_| async  {
        let new_duel = collection.new_duel().await.unwrap();
        let home_value: u32 = new_duel.home.parse().unwrap();
        let guest_value: u32 = new_duel.guest.parse().unwrap();
        let home_id = new_duel.home_id;
        let guest_id = new_duel.guest_id;
        let mut rng = rand::thread_rng();
        let skew = distribution.sample(&mut rng);
        
        let won = {            
            if (home_value as f64 + skew) > guest_value as f64 {
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
        collection.insert_match(&m).await;
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
