pub struct Rating {
    pub rating: f64,
    pub deviation: f64,
    pub time: usize,
}

fn time_decay(prev_rd: f64, time: usize, decay_factor: f64) -> f64 {
    let rd_sqr = prev_rd * prev_rd;
    let c_sqr = decay_factor * decay_factor;
    let cct = c_sqr * (time as f64);
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

pub fn new_rating(
    r_home: &Rating,
    r_other: &Rating,
    home_won: f64,
    time: usize,
    decay_factor: f64,
) -> Rating {
    let time_delta = (time - r_home.time).max(0);
    let rd = time_decay(r_home.deviation, time_delta, decay_factor);
    let g_rdi = g_rd(r_other.deviation);
    let e = e_f(r_home.rating, r_other.rating, r_other.deviation);
    let d_sqr_inv = Q_COEFF_SQR * g_rdi * g_rdi * e * (1_f64 - e);
    let dif = 1_f64 / (rd * rd) + d_sqr_inv;
    let dif_inv = 1_f64 / dif;
    let coeff = Q_COEFF * dif_inv;
    let new_rating = r_home.rating + coeff * (g_rdi * home_won - e);
    let new_rd = dif_inv.sqrt();
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
        assert_delta!(nr.rating, 1562.9, 0.1);
        assert_delta!(nr.deviation, 175.2, 0.1);
        assert_eq!(nr.time, 0);
    }
}
