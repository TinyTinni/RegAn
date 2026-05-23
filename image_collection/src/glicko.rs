pub struct Rating {
    pub rating: f64,
    pub deviation: f64,
    pub time: usize,
}

fn time_decay(prev_rd: f64, time: f64, decay_factor: f64) -> f64 {
    let rd_sqr = prev_rd * prev_rd;
    let c_sqr = decay_factor * decay_factor;
    let cct = c_sqr * time;
    (rd_sqr + cct).sqrt().min(350_f64)
}

const Q_COEFF: f64 = std::f64::consts::LN_10 / 400_f64;
const Q_COEFF_SQR: f64 = Q_COEFF * Q_COEFF;
const PI_SQR: f64 = std::f64::consts::PI * std::f64::consts::PI;
const G_COEFF: f64 = 3_f64 * Q_COEFF_SQR / PI_SQR;

fn g_rd(rd_opponent: f64) -> f64 {
    let rd_opp_sqr = rd_opponent * rd_opponent;
    1_f64 / (1_f64 + G_COEFF * rd_opp_sqr).sqrt()
}

fn e_f(rating_this: f64, rating_other: f64, rd_other: f64) -> f64 {
    let exp_coeff = -g_rd(rd_other) * (rating_this - rating_other) * Q_COEFF;
    let dif = exp_coeff.exp();
    1_f64 / (1_f64 + dif)
}

/// Glicko-1 rating algorithm (Glickman, 1995).
/// Operates on raw rating/scale directly (not the reduced Glicko-2 scale).
pub fn new_rating(
    r_home: &Rating,
    r_other: &Rating,
    home_won: f64,
    time: usize,
    decay_factor: f64,
) -> Rating {
    let time_delta = (time as f64) - (r_home.time as f64);
    let rd = time_decay(r_home.deviation, time_delta, decay_factor);
    let g_rdi = g_rd(r_other.deviation);
    let e = e_f(r_home.rating, r_other.rating, r_other.deviation);
    let d_sqr_inv = Q_COEFF_SQR * g_rdi * g_rdi * e * (1_f64 - e);
    let dif = 1_f64 / (rd * rd) + d_sqr_inv;
    let dif_inv = 1_f64 / dif;
    let coeff = Q_COEFF * dif_inv;
    let home_won = home_won.clamp(0.0, 1.0);
    let new_rating = r_home.rating + coeff * g_rdi * (home_won - e);
    let new_rd = dif_inv.sqrt().min(350_f64);
    Rating {
        deviation: new_rd,
        rating: new_rating,
        time,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    macro_rules! assert_delta {
        ($x:expr_2021, $y:expr_2021, $d:expr_2021) => {
            assert!(($x - $y).abs() < $d && ($y - $x).abs() < $d);
        };
    }
    #[test]
    fn test_g() {
        let g = g_rd(30_f64);
        assert_delta!(g, 0.9954980060779481_f64, 0.00001);
        let g = g_rd(100_f64);
        assert_delta!(g, 0.953148974234587_f64, 0.00001);
    }

    #[test]
    fn test_e() {
        let e = e_f(1500.0_f64, 1400_f64, 30.0_f64);
        assert_delta!(e, 0.639467736007921, 0.00001);
    }

    #[test]
    fn test_new_rating() {
        let h = Rating {
            rating: 1500.0,
            deviation: 200.0,
            time: 0,
        };
        let g = Rating {
            rating: 1400.0,
            deviation: 30.0,
            time: 0,
        };
        let nr = new_rating(&h, &g, 1.0, 0, 0_f64);
        assert_delta!(nr.rating, 1563.43, 0.1);
        assert_delta!(nr.deviation, 175.22, 0.1);
        assert_eq!(nr.time, 0);
    }

    #[test]
    fn test_time_decay_no_decay() {
        let rd = time_decay(200.0, 0.0, 0.0);
        assert_delta!(rd, 200.0, 1e-12);
    }

    #[test]
    fn test_time_decay_increases_rd() {
        let rd = time_decay(200.0, 100.0, 5.0);
        let expected = (200.0_f64 * 200.0 + 25.0 * 100.0).sqrt();
        assert_delta!(rd, expected, 0.001);
        assert!(rd > 200.0);
        assert!(rd <= 350.0);
    }

    #[test]
    fn test_time_decay_cap_at_350() {
        let rd = time_decay(200.0, 10_000.0, 5.0);
        assert_delta!(rd, 350.0, 1e-12);
    }

    #[test]
    fn test_g_rd_zero_and_max() {
        assert_delta!(g_rd(0.0), 1.0, 1e-12);
        let g_max = g_rd(350.0);
        assert!(g_max > 0.0);
        assert!(g_max < 1.0);
    }

    #[test]
    fn test_e_f_equal_ratings() {
        let e = e_f(1500.0, 1500.0, 30.0);
        assert_delta!(e, 0.5, 1e-12);
        let e = e_f(1500.0, 1500.0, 350.0);
        assert_delta!(e, 0.5, 1e-12);
    }

    #[test]
    fn test_new_rating_loss_is_inverse_of_win() {
        let a = Rating {
            rating: 1500.0,
            deviation: 200.0,
            time: 0,
        };
        let b = Rating {
            rating: 1500.0,
            deviation: 200.0,
            time: 0,
        };
        let win = new_rating(&a, &b, 1.0, 0, 0_f64);
        let loss = new_rating(&a, &b, 0.0, 0, 0_f64);
        assert_delta!(win.rating - a.rating, -(loss.rating - a.rating), 0.001);
        assert_delta!(win.deviation, loss.deviation, 1e-12);
    }

    #[test]
    fn test_new_rating_clamps_input() {
        let h = Rating {
            rating: 1500.0,
            deviation: 200.0,
            time: 0,
        };
        let g = Rating {
            rating: 1400.0,
            deviation: 30.0,
            time: 0,
        };
        let win = new_rating(&h, &g, 1.0, 0, 0_f64);
        let overshoot = new_rating(&h, &g, 2.0, 0, 0_f64);
        assert_eq!(win.rating, overshoot.rating);
        assert_eq!(win.deviation, overshoot.deviation);
        let loss = new_rating(&h, &g, 0.0, 0, 0_f64);
        let undershoot = new_rating(&h, &g, -1.0, 0, 0_f64);
        assert_eq!(loss.rating, undershoot.rating);
        assert_eq!(loss.deviation, undershoot.deviation);
    }

    #[test]
    fn test_new_rating_deviation_never_exceeds_350() {
        let scenarios = [
            (1500.0, 200.0, 1400.0, 30.0, 1.0),
            (1500.0, 350.0, 1500.0, 350.0, 0.5),
            (1500.0, 350.0, 1500.0, 350.0, 0.0),
            (1500.0, 350.0, 1500.0, 350.0, 1.0),
            (2200.0, 350.0, 1800.0, 50.0, 1.0),
            (1000.0, 100.0, 2000.0, 300.0, 0.0),
        ];
        for (r_h, rd_h, r_g, rd_g, s) in scenarios {
            let h = Rating {
                rating: r_h,
                deviation: rd_h,
                time: 0,
            };
            let g = Rating {
                rating: r_g,
                deviation: rd_g,
                time: 0,
            };
            let nr = new_rating(&h, &g, s, 0, 0_f64);
            assert!(
                nr.deviation <= 350.0,
                "deviation {0} exceeds 350",
                nr.deviation
            );
            assert!(nr.deviation >= 0.0);
        }
    }
}
