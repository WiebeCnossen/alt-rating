use crate::model::PeriodResult;
use std::error::Error;
use std::path::{Path, PathBuf};
use tokio::fs;

const CACHE_DIR: &str = "cache";

pub fn period_cache_path(fide_id: &str, period: &str) -> PathBuf {
    PathBuf::from(CACHE_DIR).join(format!("{fide_id}_{period}.json"))
}

pub fn standard_rating_list_path(year: i32, month: u32) -> PathBuf {
    PathBuf::from(CACHE_DIR).join(format!("standard-{year:04}-{month:02}.txt"))
}

pub async fn save_standard_rating_list(
    year: i32,
    month: u32,
    text: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(CACHE_DIR).await?;
    let path = standard_rating_list_path(year, month);
    fs::write(&path, text).await?;
    Ok(path)
}

pub async fn load_period_result(
    path: impl AsRef<Path>,
    fide_id: &str,
    period: &str,
) -> Option<PeriodResult> {
    let text = fs::read_to_string(path).await.ok()?;
    let result: PeriodResult = serde_json::from_str(&text).ok()?;
    (result.fide_id == fide_id && result.period == period).then_some(result)
}

pub async fn save_period_result(
    path: impl AsRef<Path>,
    result: &PeriodResult,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(CACHE_DIR).await?;
    let text = serde_json::to_string_pretty(result)?;
    fs::write(path, text).await?;
    Ok(())
}

/// Load each period from cache; `None` entries are cache misses.
pub async fn load_cached_period_results(
    fide_id: &str,
    periods: &[String],
) -> Vec<Option<PeriodResult>> {
    let mut results = Vec::with_capacity(periods.len());
    for period in periods {
        let path = period_cache_path(fide_id, period);
        results.push(load_period_result(&path, fide_id, period).await);
    }
    results
}

pub async fn remove_period_cache(path: impl AsRef<Path>) {
    let _ = fs::remove_file(path).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn period_cache_path_uses_fide_id_and_period() {
        assert_eq!(
            period_cache_path("1503014", "2025-09-01"),
            PathBuf::from("cache").join("1503014_2025-09-01.json")
        );
    }

    #[test]
    fn standard_rating_list_path_uses_year_month() {
        assert_eq!(
            standard_rating_list_path(2026, 9),
            PathBuf::from("cache").join("standard-2026-09.txt")
        );
    }
}
