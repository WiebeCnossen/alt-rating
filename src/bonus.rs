use crate::model::{CompactPlayer, PeriodResult};
use crate::rating_list::{parse_period, period_minus_months};
use chrono::{Datelike, Months, NaiveDate};
use std::collections::HashMap;
use std::error::Error;

pub const TOP_N: u32 = 30;
pub const MIN_ACTIVE_GAMES: u32 = 20;
const BONUS_RANK_1: f64 = 3.0;
const BONUS_RANK_30: f64 = 0.1;

#[derive(Clone, Debug)]
struct RankedOpponent {
    rating: u32,
    rank: u32,
}

/// Active top-30 index for one list month, keyed by normalized name.
#[derive(Clone, Debug, Default)]
pub struct Top30Month {
    by_name: HashMap<String, Vec<RankedOpponent>>,
}

/// Top-30 indexes keyed by list period `YYYY-MM-01`.
#[derive(Clone, Debug, Default)]
pub struct Top30ByMonth {
    months: HashMap<String, Top30Month>,
}

impl Top30ByMonth {
    pub fn get(&self, list_period: &str) -> Option<&Top30Month> {
        self.months.get(list_period)
    }

    pub fn insert(&mut self, list_period: String, month: Top30Month) {
        self.months.insert(list_period, month);
    }
}

/// Linear bonus for defeating a player at `rank` (1..=30). Others → 0.
pub fn bonus_for_rank(rank: u32) -> f64 {
    if rank == 0 || rank > TOP_N {
        return 0.0;
    }
    BONUS_RANK_1 - (rank as f64 - 1.0) * (BONUS_RANK_1 - BONUS_RANK_30) / (TOP_N as f64 - 1.0)
}

pub fn normalize_name(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Build an active top-30 index for list month `list_period` (`YYYY-MM-01`).
///
/// `lists_by_period` must contain compact players for `list_period` and the
/// 11 preceding months (12 months total) so activity can be summed.
pub fn build_top30_month(
    list_period: &str,
    sex: char,
    lists_by_period: &HashMap<String, Vec<CompactPlayer>>,
) -> Result<Top30Month, Box<dyn Error>> {
    let activity_periods = activity_window_periods(list_period)?;
    let mut games_by_id: HashMap<String, u32> = HashMap::new();
    for period in &activity_periods {
        let Some(players) = lists_by_period.get(period) else {
            return Err(format!("missing compact rating list for {period}").into());
        };
        for p in players {
            if p.sex != sex {
                continue;
            }
            *games_by_id.entry(p.fide_id.clone()).or_default() += p.gms;
        }
    }

    let Some(current) = lists_by_period.get(list_period) else {
        return Err(format!("missing compact rating list for {list_period}").into());
    };

    let mut active: Vec<&CompactPlayer> = current
        .iter()
        .filter(|p| {
            p.sex == sex && games_by_id.get(&p.fide_id).copied().unwrap_or(0) >= MIN_ACTIVE_GAMES
        })
        .collect();
    active.sort_by(|a, b| {
        b.rating
            .cmp(&a.rating)
            .then_with(|| a.fide_id.cmp(&b.fide_id))
    });

    let ranked = competition_ranks(&active);
    let mut by_name: HashMap<String, Vec<RankedOpponent>> = HashMap::new();
    for (player, rank) in ranked {
        if rank > TOP_N {
            break;
        }
        by_name
            .entry(normalize_name(&player.name))
            .or_default()
            .push(RankedOpponent {
                rating: player.rating,
                rank,
            });
    }
    Ok(Top30Month { by_name })
}

/// Competition ranks: ties share the best (lowest) rank; next rank skips.
fn competition_ranks<'a>(sorted_desc: &[&'a CompactPlayer]) -> Vec<(&'a CompactPlayer, u32)> {
    let mut out = Vec::with_capacity(sorted_desc.len());
    let mut rank = 1u32;
    for (i, player) in sorted_desc.iter().enumerate() {
        if i > 0 && player.rating < sorted_desc[i - 1].rating {
            rank = (i as u32) + 1;
        }
        out.push((*player, rank));
    }
    out
}

fn activity_window_periods(list_period: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let end = NaiveDate::parse_from_str(list_period, "%Y-%m-%d")?;
    let mut periods = Vec::with_capacity(12);
    for i in (0..12).rev() {
        let d = end
            .checked_sub_months(Months::new(i))
            .ok_or("activity window underflow")?;
        periods.push(format!("{:04}-{:02}-01", d.year(), d.month()));
    }
    Ok(periods)
}

impl Top30Month {
    pub fn rank_of(&self, opponent_name: &str, opponent_rating: u32) -> Option<u32> {
        let candidates = self.by_name.get(&normalize_name(opponent_name))?;
        match candidates.len() {
            0 => None,
            1 => Some(candidates[0].rank),
            _ => candidates
                .iter()
                .find(|c| c.rating == opponent_rating)
                .map(|c| c.rank),
        }
    }
}

/// Sum win bonuses for all games in `results` against the active top 30.
///
/// A game in period `P` uses the top-30 list for period `P−1`.
pub fn year_win_bonus(
    results: &[PeriodResult],
    top30: &Top30ByMonth,
) -> Result<f64, Box<dyn Error>> {
    let mut total = 0.0;
    for period in results {
        let list_period = period_minus_months(&period.period, 1)?;
        let Some(month) = top30.get(&list_period) else {
            // No top-30 data for this month: skip (caller should preload).
            continue;
        };
        for game in &period.games {
            let score: f64 = game.result.trim().parse()?;
            if (score - 1.0).abs() > 1e-9 {
                continue;
            }
            let rating: u32 = game.opponent_rating.trim().parse()?;
            if let Some(rank) = month.rank_of(&game.opponent_name, rating) {
                total += bonus_for_rank(rank);
            }
        }
    }
    Ok(total)
}

/// List months needed for top-30 lookups over the given game periods:
/// for each game period P, list P−1 plus its 11 preceding months for activity.
pub fn required_list_months(game_periods: &[String]) -> Result<Vec<(i32, u32)>, Box<dyn Error>> {
    let mut set = std::collections::BTreeSet::new();
    for period in game_periods {
        let list_period = period_minus_months(period, 1)?;
        for offset in 0..12u32 {
            let p = period_minus_months(&list_period, offset)?;
            set.insert(parse_period(&p)?);
        }
    }
    Ok(set.into_iter().collect())
}

/// Build top-30 indexes for every list month P−1 implied by `game_periods`.
pub fn build_top30_for_periods(
    game_periods: &[String],
    sex: char,
    lists_by_period: &HashMap<String, Vec<CompactPlayer>>,
) -> Result<Top30ByMonth, Box<dyn Error>> {
    let mut out = Top30ByMonth::default();
    let mut list_periods = std::collections::BTreeSet::new();
    for period in game_periods {
        list_periods.insert(period_minus_months(period, 1)?);
    }
    for list_period in list_periods {
        let month = build_top30_month(&list_period, sex, lists_by_period)?;
        out.insert(list_period, month);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Game;

    fn compact(id: &str, name: &str, sex: char, rating: u32, gms: u32) -> CompactPlayer {
        CompactPlayer {
            fide_id: id.into(),
            name: name.into(),
            sex,
            rating,
            gms,
        }
    }

    fn period_games(period: &str, games: Vec<Game>) -> PeriodResult {
        PeriodResult {
            fide_id: "1".into(),
            period: period.into(),
            games,
        }
    }

    fn win(name: &str, rating: u32) -> Game {
        Game {
            color: "white".into(),
            opponent_name: name.into(),
            opponent_rating: rating.to_string(),
            result: "1.00".into(),
        }
    }

    fn draw(name: &str, rating: u32) -> Game {
        Game {
            color: "white".into(),
            opponent_name: name.into(),
            opponent_rating: rating.to_string(),
            result: "0.50".into(),
        }
    }

    /// 12 identical months so activity = 12 * gms_per_month.
    fn lists_with_constant_gms(
        players: &[CompactPlayer],
        end_period: &str,
    ) -> HashMap<String, Vec<CompactPlayer>> {
        let mut map = HashMap::new();
        for offset in 0..12u32 {
            let p = period_minus_months(end_period, offset).unwrap();
            map.insert(p, players.to_vec());
        }
        map
    }

    #[test]
    fn bonus_formula_endpoints_and_mid() {
        assert!((bonus_for_rank(1) - 3.0).abs() < 1e-9);
        assert!((bonus_for_rank(30) - 0.1).abs() < 1e-9);
        let mid = bonus_for_rank(15);
        let expected = 3.0 - 14.0 * 2.9 / 29.0;
        assert!((mid - expected).abs() < 1e-9);
        assert_eq!(bonus_for_rank(0), 0.0);
        assert_eq!(bonus_for_rank(31), 0.0);
    }

    #[test]
    fn competition_ties_share_best_rank() {
        let players = [
            compact("1", "A", 'M', 2800, 2),
            compact("2", "B", 'M', 2700, 2),
            compact("3", "C", 'M', 2700, 2),
            compact("4", "D", 'M', 2600, 2),
        ];
        let refs: Vec<&CompactPlayer> = players.iter().collect();
        let ranked = competition_ranks(&refs);
        assert_eq!(ranked[0].1, 1);
        assert_eq!(ranked[1].1, 2);
        assert_eq!(ranked[2].1, 2);
        assert_eq!(ranked[3].1, 4);
    }

    #[test]
    fn inactive_players_excluded_from_top30() {
        // gms=1 per month → 12 over the window (< 20) for Low; High has 2 → 24.
        let end = "2026-06-01";
        let players = vec![
            compact("1", "High, Active", 'M', 2800, 2),
            compact("2", "Low, Inactive", 'M', 2790, 1),
        ];
        let lists = lists_with_constant_gms(&players, end);
        let top = build_top30_month(end, 'M', &lists).unwrap();
        assert_eq!(top.rank_of("High, Active", 2800), Some(1));
        assert_eq!(top.rank_of("Low, Inactive", 2790), None);
    }

    #[test]
    fn year_bonus_uses_p_minus_1_and_only_wins() {
        let list_month = "2026-06-01";
        let mut players = Vec::new();
        for i in 0..30u32 {
            players.push(compact(
                &format!("{i}"),
                &format!("Player, {i}"),
                'M',
                2800 - i,
                2,
            ));
        }
        let lists = lists_with_constant_gms(&players, list_month);
        let mut top30 = Top30ByMonth::default();
        top30.insert(
            list_month.into(),
            build_top30_month(list_month, 'M', &lists).unwrap(),
        );

        let results = vec![period_games(
            "2026-07-01",
            vec![
                win("Player, 0", 2800), // rank 1 → 3.0
                draw("Player, 1", 2799),
                win("Player, 29", 2771), // rank 30 → 0.1
            ],
        )];
        let sum = year_win_bonus(&results, &top30).unwrap();
        assert!((sum - 3.1).abs() < 1e-9);
    }

    #[test]
    fn tied_rank_uses_shared_bonus() {
        let end = "2026-06-01";
        let mut players = vec![compact("1", "One", 'M', 2800, 2)];
        // Two tied for #2
        players.push(compact("2", "TwoA", 'M', 2700, 2));
        players.push(compact("3", "TwoB", 'M', 2700, 2));
        for i in 0..27u32 {
            players.push(compact(
                &format!("x{i}"),
                &format!("Fill, {i}"),
                'M',
                2600 - i,
                2,
            ));
        }
        let lists = lists_with_constant_gms(&players, end);
        let top = build_top30_month(end, 'M', &lists).unwrap();
        assert_eq!(top.rank_of("TwoA", 2700), Some(2));
        assert_eq!(top.rank_of("TwoB", 2700), Some(2));
        assert!((bonus_for_rank(2) - (3.0 - 2.9 / 29.0)).abs() < 1e-9);
    }

    #[test]
    fn required_list_months_covers_activity_window() {
        let months = required_list_months(&["2026-07-01".into()]).unwrap();
        // Game period Jul → list Jun; activity Jun back 11 months → Jul 2025 .. Jun 2026
        assert_eq!(months.first().copied().unwrap(), (2025, 7));
        assert_eq!(months.last().copied().unwrap(), (2026, 6));
        assert_eq!(months.len(), 12);
    }
}
