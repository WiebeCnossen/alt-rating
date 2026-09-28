use crate::http::fetch_bytes_with_retry;
use crate::model::Player;
use std::error::Error;
use std::io::{Cursor, Read};
use zip::ZipArchive;

/// Full standard rating list (zipped fixed-width text).
const STANDARD_RATING_LIST_URL: &str = "https://ratings.fide.com/download/standard_rating_list.zip";

/// Fixed-width columns in `standard_rating_list.txt`.
const COL_ID: std::ops::Range<usize> = 0..15;
const COL_NAME: std::ops::Range<usize> = 15..76;
const COL_SEX: usize = 80;
const COL_RATING: std::ops::Range<usize> = 113..119;
const COL_FLAG_START: usize = 132;

/// Download the standard rating list and return active men and women, highest rating first.
pub async fn fetch_top_players() -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let bytes =
        fetch_bytes_with_retry(STANDARD_RATING_LIST_URL, |bytes| bytes.starts_with(b"PK")).await?;
    parse_top_players_from_zip(&bytes)
}

fn parse_top_players_from_zip(bytes: &[u8]) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    let mut file = archive.by_index(0)?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    let text = String::from_utf8_lossy(&raw);
    parse_top_players_from_rating_list(&text)
}

fn parse_top_players_from_rating_list(
    text: &str,
) -> Result<(Vec<Player>, Vec<Player>), Box<dyn Error>> {
    let mut men = Vec::new();
    let mut women = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if idx == 0 || line.len() < COL_FLAG_START {
            continue;
        }
        let flag = line[COL_FLAG_START..].trim();
        if flag.contains('i') {
            continue;
        }
        let sex = line.as_bytes().get(COL_SEX).copied().unwrap_or(b' ');
        let rating = line[COL_RATING].trim();
        if rating.is_empty() || rating.parse::<u32>().is_err() {
            continue;
        }
        let player = Player {
            fide_id: line[COL_ID].trim().to_string(),
            name: line[COL_NAME].trim().to_string(),
            rating: rating.to_string(),
        };
        match sex {
            b'M' => men.push(player),
            b'F' => women.push(player),
            _ => {}
        }
    }

    let by_rating_desc = |a: &Player, b: &Player| {
        b.rating
            .parse::<u32>()
            .unwrap_or(0)
            .cmp(&a.rating.parse::<u32>().unwrap_or(0))
    };
    men.sort_by(by_rating_desc);
    women.sort_by(by_rating_desc);

    if men.is_empty() || women.is_empty() {
        return Err("rating list did not yield players".into());
    }
    Ok((men, women))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating_list_line(id: &str, name: &str, sex: char, rating: u32, flag: &str) -> String {
        let mut line = vec![b' '; 136];
        let put = |line: &mut [u8], start: usize, value: &str| {
            let bytes = value.as_bytes();
            line[start..start + bytes.len()].copy_from_slice(bytes);
        };
        put(&mut line, 0, id);
        put(&mut line, 15, name);
        line[80] = sex as u8;
        put(&mut line, 113, &format!("{rating}"));
        put(&mut line, 132, flag);
        String::from_utf8(line).unwrap()
    }

    #[test]
    fn parses_rating_list_skipping_inactive_and_sorting_by_rating() {
        let text = [
            "ID Number      Name                                                         Fed Sex Tit  WTit OTit           FOA SEP26 Gms K  B-day Flag".to_string(),
            rating_list_line("1", "Low, Man", 'M', 2000, ""),
            rating_list_line("2", "Top, Man", 'M', 2800, ""),
            rating_list_line("3", "Inactive, Man", 'M', 2900, "i"),
            rating_list_line("4", "Near, Man", 'M', 2550, ""),
            rating_list_line("5", "Top, Woman", 'F', 2500, "w"),
            rating_list_line("6", "Inactive, Woman", 'F', 2600, "wi"),
            rating_list_line("7", "Second, Woman", 'F', 2400, ""),
            rating_list_line("8", "Far, Woman", 'F', 2200, ""),
        ]
        .join("\n");

        let (men, women) = parse_top_players_from_rating_list(&text).unwrap();
        assert_eq!(
            men.iter().map(|p| p.fide_id.as_str()).collect::<Vec<_>>(),
            vec!["2", "4", "1"]
        );
        assert_eq!(
            women.iter().map(|p| p.fide_id.as_str()).collect::<Vec<_>>(),
            vec!["5", "7", "8"]
        );
        assert!(men.iter().all(|p| p.fide_id != "3"));
        assert!(women.iter().all(|p| p.fide_id != "6"));
    }
}
