#[derive(Clone)]
pub struct Player {
    pub fide_id: String,
    pub name: String,
    pub rating: String,
}

/// Compact rating-list row used for historical top-30 / activity lookups.
#[derive(Clone, Debug)]
pub struct CompactPlayer {
    pub fide_id: String,
    pub name: String,
    /// `M` or `F`.
    pub sex: char,
    pub rating: u32,
    pub gms: u32,
}

#[derive(Clone, Debug)]
pub struct Game {
    pub color: String,
    /// Opponent display name as shown on the FIDE calculations page.
    pub opponent_name: String,
    pub opponent_rating: String,
    pub result: String,
}

#[derive(Clone, Debug)]
pub struct PeriodResult {
    pub fide_id: String,
    /// FIDE rating period as `YYYY-MM-01`: games rated into the list as of that date.
    pub period: String,
    pub games: Vec<Game>,
}

pub struct PlayerSummary {
    pub name: String,
    pub fide_id: String,
    pub rating: String,
    pub elo_year: i32,
    /// Sum of top-30 win bonuses for the year (before rounding into `elo_year`).
    pub bonus: f64,
    pub all_games_tpr: i32,
    pub raw_games_tpr: i32,
    pub total_games: usize,
}

/// Player whose period games could not be fully loaded (e.g. fetch retries exhausted).
#[derive(Clone)]
pub struct IncompletePlayer {
    pub name: String,
    pub fide_id: String,
    pub rating: String,
}

impl PlayerSummary {
    pub fn diff(&self) -> i32 {
        self.elo_year - self.rating.parse::<i32>().unwrap_or(0)
    }
}
