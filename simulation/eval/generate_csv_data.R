###### Configuration
# Number of games which should be estimated
games <- seq(2500, 2500, 100)

# Number of samples you got to rate
samples <- 700

# uncertainty
# how much errors a human makes when he compares samples
# which are very close to each other
std_dev <- 100.0


# path to the program. 
# The Rust program should be compiled first with "cargo build --release"
if (require(here)) {
  library(here)
  here::i_am("simulation/eval/eval.Rproj")
  program_path <- here("target", "release", "simulation")
} else {
  program_path <- "../../target/release/simulation"
}


#####################

simulate_glicko <- function(samples, games, mu, program_path) {
  csv <-
    system2(program_path, paste0(
      "-g ",
      format(games, scientific = FALSE),
      " -s ",
      format(samples, scientific = FALSE),
      " --std-dev ",
      format(std_dev, scientific = FALSE)
    ), stdout = TRUE)
  df <- read.csv(textConnection(csv))

  df$linear_rank <- seq(1:samples)

  df$places_diff = df$original - df$linear_rank

  df
}

library(ggplot2)
for (g in games) {
  df <- simulate_glicko(samples, g, 0.0, program_path)
  msre <- sqrt(sum(df$places_diff * df$places_diff) / nrow(df))

  ## graphical output
  title <- paste0(
    "games: ",
    format(g, scientific = F),
    " samples: ",
    format(samples, scientific = F),
    " MSRE: ",
    round(msre, digits = 2),
    " avg. deviation: ",
    round(mean(df$deviation), digits = 2)
  )
  p <-
    ggplot(
      df,
      aes(
        x = original,
        y = rating,
        ymin = rating - 1.96 * deviation,
        ymax = rating + 1.96 * deviation
      )
    ) +
    geom_point() +
    geom_pointrange(colour = "#000099") +
    ggtitle(title)

  p_linear <- ggplot(df, aes(x = original, y = linear_rank)) +
    geom_point(colour = "#FF0000") +
    ggtitle(title)

  print(p_linear)
  Sys.sleep(1)
}
