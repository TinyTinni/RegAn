use anyhow::Result;
use clap::Parser;
use image_collection::{ImageCollection, Match, MatchOutcome, Strategy};
use rand::SeedableRng;
use rand::rngs::StdRng;
use rand_distr::{Distribution, Normal};

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

    /// The matchmaking strategy: "windowed" (rating +/- deviation window)
    /// or "uniform" (fully random opponents)
    #[clap(long, default_value = "windowed")]
    strategy: String,

    /// Seed for all randomness of the run.
    /// The same seed together with the same arguments yields the same results.
    /// When omitted, a random seed is chosen and printed to stderr.
    #[clap(long)]
    seed: Option<u64>,

    /// Prints timings for program runtime
    #[clap(long, default_value_t = false, value_parser)]
    print_timing: bool,

    /// Omit printing resulting CSV
    #[clap(long, default_value_t = false, value_parser)]
    no_csv: bool,
}

async fn run_simulation(
    samples: usize,
    games: usize,
    std_dev: f64,
    seed: u64,
    strategy: Strategy,
) -> Result<ImageCollection> {
    let mut collection = ImageCollection::new_pre_configured(samples as u32, seed).await?;
    collection.set_strategy(strategy);

    let distribution = Normal::new(0_f64, std_dev)?;
    // different offset than the collection's batch counter so the outcome
    // noise stream stays independent of the matchmaking stream
    let mut rng = StdRng::seed_from_u64(seed.wrapping_add(0xD1B5_4A32_D192_ED03));

    for _ in 0..games {
        let new_duel = collection.new_duel().await.unwrap();
        let home_value = new_duel.home.parse::<u32>().unwrap();
        let guest_value = new_duel.guest.parse::<u32>().unwrap();
        let home_id = new_duel.home_id;
        let guest_id = new_duel.guest_id;
        let skew = distribution.sample(&mut rng);

        let won = if (home_value as f64 + skew) > guest_value as f64 {
            MatchOutcome::HomeWin
        } else {
            MatchOutcome::GuestWin
        };
        let m = Match {
            home_id,
            guest_id,
            won,
        };
        collection.insert_match(m).await;
    }
    Ok(collection)
}

/// runs a full simulation and returns its final ranking error (`msre`)
#[cfg(test)]
async fn run_and_score(
    samples: usize,
    games: usize,
    std_dev: f64,
    seed: u64,
    strategy: Strategy,
) -> Result<f32> {
    let collection = run_simulation(samples, games, std_dev, seed, strategy).await?;
    collection.msre().await
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    env_logger::builder()
        //.filter_level(log::LevelFilter::Warn)
        .init();

    let args = Args::parse();
    let strategy: Strategy = args.strategy.parse().map_err(anyhow::Error::msg)?;
    let seed = args.seed.unwrap_or_else(rand::random::<u64>);
    if args.seed.is_none() {
        eprintln!("no --seed given, using seed {seed} (rerun with --seed {seed} to reproduce)");
    }

    let start = std::time::Instant::now();
    let collection = run_simulation(args.samples, args.games, args.std_dev, seed, strategy).await?;

    if !args.no_csv {
        collection.print_csv().await?;
    }

    if args.print_timing {
        let runs_per_sec = args.games as f64 / start.elapsed().as_secs_f64();
        println!("runs per sec: {runs_per_sec}");
    }

    Ok(())
}

#[cfg(test)]
mod simulation {
    use super::*;

    /// baseline: the implemented strategy must keep the absolute ranking error in bounds
    #[tokio::test]
    async fn regression() {
        let msre = run_and_score(500, 5000, 50.0, 42, Strategy::Windowed)
            .await
            .unwrap();
        eprintln!("seed 42: windowed msre {msre}");
        assert!(msre < 25.5, "msre: {msre}");
    }

    /// relative check: windowed matchmaking must not lose against uniform pairings
    /// under identical conditions (same seed => only the strategy differs)
    #[tokio::test]
    async fn windowed_strategy_is_not_worse_than_uniform() {
        let seed = 7;
        let windowed = run_and_score(500, 5000, 50.0, seed, Strategy::Windowed)
            .await
            .unwrap();
        let uniform = run_and_score(500, 5000, 50.0, seed, Strategy::Uniform)
            .await
            .unwrap();
        eprintln!("seed {seed}: windowed msre {windowed}, uniform msre {uniform}");
        assert!(
            windowed <= uniform + 1.0,
            "windowed msre {windowed} vs uniform msre {uniform}"
        );
    }

    /// the same seed must reproduce the exact same run
    #[tokio::test]
    async fn seeded_runs_are_reproducible() {
        let a = run_and_score(200, 1000, 50.0, 99, Strategy::Windowed)
            .await
            .unwrap();
        let b = run_and_score(200, 1000, 50.0, 99, Strategy::Windowed)
            .await
            .unwrap();
        assert_eq!(a, b, "same seed must yield identical results");
    }
}
