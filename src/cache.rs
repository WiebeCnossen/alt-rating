use crate::model::{CompactPlayer, Game, PeriodResult};
use rusqlite::{Connection, OptionalExtension, params};
use std::error::Error;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const DEFAULT_DB_PATH: &str = "cache/alt-rating.db";

static CACHE: OnceLock<Mutex<CacheDb>> = OnceLock::new();

/// SQLite-backed cache for period games and rating-list players.
pub struct CacheDb {
    conn: Connection,
}

impl CacheDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn Error>> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;
            ",
        )?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<(), Box<dyn Error>> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS period_results (
                fide_id TEXT NOT NULL,
                period TEXT NOT NULL,
                PRIMARY KEY (fide_id, period)
            );

            CREATE TABLE IF NOT EXISTS period_games (
                fide_id TEXT NOT NULL,
                period TEXT NOT NULL,
                game_index INTEGER NOT NULL,
                color TEXT NOT NULL,
                opponent_name TEXT NOT NULL,
                opponent_rating TEXT NOT NULL,
                result TEXT NOT NULL,
                PRIMARY KEY (fide_id, period, game_index),
                FOREIGN KEY (fide_id, period)
                    REFERENCES period_results(fide_id, period)
                    ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS rating_lists (
                year INTEGER NOT NULL,
                month INTEGER NOT NULL,
                PRIMARY KEY (year, month)
            );

            CREATE TABLE IF NOT EXISTS rating_list_players (
                year INTEGER NOT NULL,
                month INTEGER NOT NULL,
                fide_id TEXT NOT NULL,
                name TEXT NOT NULL,
                sex TEXT NOT NULL,
                rating INTEGER NOT NULL,
                gms INTEGER NOT NULL,
                PRIMARY KEY (year, month, fide_id),
                FOREIGN KEY (year, month)
                    REFERENCES rating_lists(year, month)
                    ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_rating_list_players_rating
                ON rating_list_players(year, month, rating);
            ",
        )?;
        Ok(())
    }

    pub fn load_period_result(&self, fide_id: &str, period: &str) -> Option<PeriodResult> {
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM period_results WHERE fide_id = ?1 AND period = ?2",
                params![fide_id, period],
                |_| Ok(true),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false);
        if !exists {
            return None;
        }

        let mut stmt = self
            .conn
            .prepare(
                "SELECT color, opponent_name, opponent_rating, result
                 FROM period_games
                 WHERE fide_id = ?1 AND period = ?2
                 ORDER BY game_index",
            )
            .ok()?;
        let games = stmt
            .query_map(params![fide_id, period], |row| {
                Ok(Game {
                    color: row.get(0)?,
                    opponent_name: row.get(1)?,
                    opponent_rating: row.get(2)?,
                    result: row.get(3)?,
                })
            })
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;

        if games.iter().any(|g| g.opponent_name.trim().is_empty()) {
            return None;
        }

        Some(PeriodResult {
            fide_id: fide_id.to_string(),
            period: period.to_string(),
            games,
        })
    }

    pub fn save_period_result(&self, result: &PeriodResult) -> Result<(), Box<dyn Error>> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM period_results WHERE fide_id = ?1 AND period = ?2",
            params![result.fide_id, result.period],
        )?;
        tx.execute(
            "INSERT INTO period_results (fide_id, period) VALUES (?1, ?2)",
            params![result.fide_id, result.period],
        )?;
        for (i, game) in result.games.iter().enumerate() {
            tx.execute(
                "INSERT INTO period_games
                    (fide_id, period, game_index, color, opponent_name, opponent_rating, result)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    result.fide_id,
                    result.period,
                    i as i64,
                    game.color,
                    game.opponent_name,
                    game.opponent_rating,
                    game.result,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn remove_period_result(&self, fide_id: &str, period: &str) -> Result<(), Box<dyn Error>> {
        self.conn.execute(
            "DELETE FROM period_results WHERE fide_id = ?1 AND period = ?2",
            params![fide_id, period],
        )?;
        Ok(())
    }

    pub fn has_rating_list(&self, year: i32, month: u32) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM rating_lists WHERE year = ?1 AND month = ?2",
                params![year, month as i64],
                |_| Ok(true),
            )
            .optional()
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    pub fn load_rating_list_players(
        &self,
        year: i32,
        month: u32,
        min_rating: Option<u32>,
    ) -> Option<Vec<CompactPlayer>> {
        if !self.has_rating_list(year, month) {
            return None;
        }
        let min = min_rating.unwrap_or(0);
        let mut stmt = self
            .conn
            .prepare(
                "SELECT fide_id, name, sex, rating, gms
                 FROM rating_list_players
                 WHERE year = ?1 AND month = ?2 AND rating >= ?3
                 ORDER BY rating DESC, fide_id",
            )
            .ok()?;
        let players = stmt
            .query_map(params![year, month as i64, min as i64], |row| {
                let sex: String = row.get(2)?;
                let sex = sex.chars().next().unwrap_or(' ');
                Ok(CompactPlayer {
                    fide_id: row.get(0)?,
                    name: row.get(1)?,
                    sex,
                    rating: row.get::<_, i64>(3)? as u32,
                    gms: row.get::<_, i64>(4)? as u32,
                })
            })
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(players)
    }

    /// Games (`Gms`) for `fide_id` on the given rating-list month.
    ///
    /// Returns `None` if that month's list is not in the cache. Returns
    /// `Some(0)` if the list exists but the player is absent.
    pub fn player_gms(&self, fide_id: &str, year: i32, month: u32) -> Option<u32> {
        if !self.has_rating_list(year, month) {
            return None;
        }
        let gms = self
            .conn
            .query_row(
                "SELECT gms FROM rating_list_players
                 WHERE year = ?1 AND month = ?2 AND fide_id = ?3",
                params![year, month as i64, fide_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .ok()?
            .unwrap_or(0);
        Some(gms as u32)
    }

    /// Sum of `Gms` across months; `None` if any month's list is missing.
    pub fn player_gms_sum(&self, fide_id: &str, months: &[(i32, u32)]) -> Option<u32> {
        let mut total = 0u32;
        for &(year, month) in months {
            total = total.saturating_add(self.player_gms(fide_id, year, month)?);
        }
        Some(total)
    }

    pub fn save_rating_list_players(
        &self,
        year: i32,
        month: u32,
        players: &[CompactPlayer],
    ) -> Result<(), Box<dyn Error>> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM rating_lists WHERE year = ?1 AND month = ?2",
            params![year, month as i64],
        )?;
        tx.execute(
            "INSERT INTO rating_lists (year, month) VALUES (?1, ?2)",
            params![year, month as i64],
        )?;
        for p in players {
            tx.execute(
                "INSERT INTO rating_list_players
                    (year, month, fide_id, name, sex, rating, gms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    year,
                    month as i64,
                    p.fide_id,
                    p.name,
                    p.sex.to_string(),
                    p.rating as i64,
                    p.gms as i64,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

/// Open (or create) the default cache database and install it as the process cache.
pub fn init_cache() -> Result<(), Box<dyn Error>> {
    init_cache_at(DEFAULT_DB_PATH)
}

/// Open (or create) a cache database at `path` and install it as the process cache.
pub fn init_cache_at(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let db = CacheDb::open(path)?;
    match CACHE.set(Mutex::new(db)) {
        Ok(()) => Ok(()),
        Err(_) => Err("cache already initialized".into()),
    }
}

fn with_cache<T>(
    f: impl FnOnce(&CacheDb) -> Result<T, Box<dyn Error>>,
) -> Result<T, Box<dyn Error>> {
    let cache = CACHE
        .get()
        .ok_or("cache not initialized; call init_cache() first")?;
    let guard = cache.lock().map_err(|_| "cache lock poisoned")?;
    f(&guard)
}

fn with_cache_opt<T>(f: impl FnOnce(&CacheDb) -> Option<T>) -> Option<T> {
    let cache = CACHE.get()?;
    let guard = cache.lock().ok()?;
    f(&guard)
}

pub fn load_period_result(fide_id: &str, period: &str) -> Option<PeriodResult> {
    with_cache_opt(|db| db.load_period_result(fide_id, period))
}

pub fn save_period_result(result: &PeriodResult) -> Result<(), Box<dyn Error>> {
    with_cache(|db| db.save_period_result(result))
}

pub fn remove_period_result(fide_id: &str, period: &str) -> Result<(), Box<dyn Error>> {
    with_cache(|db| db.remove_period_result(fide_id, period))
}

pub fn has_rating_list(year: i32, month: u32) -> bool {
    with_cache_opt(|db| Some(db.has_rating_list(year, month))).unwrap_or(false)
}

pub fn load_rating_list_players(
    year: i32,
    month: u32,
    min_rating: Option<u32>,
) -> Option<Vec<CompactPlayer>> {
    with_cache_opt(|db| db.load_rating_list_players(year, month, min_rating))
}

pub fn save_rating_list_players(
    year: i32,
    month: u32,
    players: &[CompactPlayer],
) -> Result<(), Box<dyn Error>> {
    with_cache(|db| db.save_rating_list_players(year, month, players))
}

/// Games (`Gms`) for `fide_id` on the given rating-list month.
///
/// `None` if the month is not cached; `Some(0)` if the player is absent.
pub fn player_gms(fide_id: &str, year: i32, month: u32) -> Option<u32> {
    with_cache_opt(|db| db.player_gms(fide_id, year, month))
}

/// Sum of `Gms` across months; `None` if any month's list is missing.
pub fn player_gms_sum(fide_id: &str, months: &[(i32, u32)]) -> Option<u32> {
    with_cache_opt(|db| db.player_gms_sum(fide_id, months))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, CacheDb) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        let db = CacheDb::open(&path).unwrap();
        (dir, db)
    }

    fn game(name: &str, rating: &str, result: &str) -> Game {
        Game {
            color: "white".into(),
            opponent_name: name.into(),
            opponent_rating: rating.into(),
            result: result.into(),
        }
    }

    #[test]
    fn period_round_trip_including_empty() {
        let (_dir, db) = temp_db();
        let empty = PeriodResult {
            fide_id: "1".into(),
            period: "2026-01-01".into(),
            games: vec![],
        };
        db.save_period_result(&empty).unwrap();
        let loaded = db.load_period_result("1", "2026-01-01").unwrap();
        assert!(loaded.games.is_empty());

        let with_games = PeriodResult {
            fide_id: "1".into(),
            period: "2026-02-01".into(),
            games: vec![
                game("A, One", "2700", "1.00"),
                game("B, Two", "2600", "0.50"),
            ],
        };
        db.save_period_result(&with_games).unwrap();
        let loaded = db.load_period_result("1", "2026-02-01").unwrap();
        assert_eq!(loaded.games.len(), 2);
        assert_eq!(loaded.games[0].opponent_name, "A, One");
        assert_eq!(loaded.games[1].result, "0.50");
    }

    #[test]
    fn period_miss_and_remove() {
        let (_dir, db) = temp_db();
        assert!(db.load_period_result("1", "2026-01-01").is_none());
        db.save_period_result(&PeriodResult {
            fide_id: "1".into(),
            period: "2026-01-01".into(),
            games: vec![game("A", "2500", "1.00")],
        })
        .unwrap();
        db.remove_period_result("1", "2026-01-01").unwrap();
        assert!(db.load_period_result("1", "2026-01-01").is_none());
    }

    #[test]
    fn rejects_games_with_empty_opponent_name() {
        let (_dir, db) = temp_db();
        db.save_period_result(&PeriodResult {
            fide_id: "1".into(),
            period: "2026-01-01".into(),
            games: vec![game("", "2500", "1.00")],
        })
        .unwrap();
        assert!(db.load_period_result("1", "2026-01-01").is_none());
    }

    #[test]
    fn rating_list_round_trip_and_min_rating_filter() {
        let (_dir, db) = temp_db();
        let players = vec![
            CompactPlayer {
                fide_id: "1".into(),
                name: "High".into(),
                sex: 'M',
                rating: 2800,
                gms: 9,
            },
            CompactPlayer {
                fide_id: "2".into(),
                name: "Low".into(),
                sex: 'F',
                rating: 2100,
                gms: 3,
            },
        ];
        db.save_rating_list_players(2026, 10, &players).unwrap();
        assert!(db.has_rating_list(2026, 10));
        assert!(!db.has_rating_list(2026, 9));

        let all = db.load_rating_list_players(2026, 10, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].fide_id, "1");

        let compact = db.load_rating_list_players(2026, 10, Some(2200)).unwrap();
        assert_eq!(compact.len(), 1);
        assert_eq!(compact[0].fide_id, "1");

        assert_eq!(db.player_gms("1", 2026, 10), Some(9));
        assert_eq!(db.player_gms("missing", 2026, 10), Some(0));
        assert_eq!(db.player_gms("1", 2026, 9), None);

        db.save_rating_list_players(
            2026,
            9,
            &[CompactPlayer {
                fide_id: "1".into(),
                name: "High".into(),
                sex: 'M',
                rating: 2800,
                gms: 4,
            }],
        )
        .unwrap();
        assert_eq!(db.player_gms_sum("1", &[(2026, 9), (2026, 10)]), Some(13));
        assert_eq!(db.player_gms_sum("1", &[(2026, 8), (2026, 10)]), None);
    }

    #[test]
    fn default_db_path_is_under_cache_dir() {
        assert_eq!(DEFAULT_DB_PATH, "cache/alt-rating.db");
    }
}
