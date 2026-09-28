use chrono::{Datelike, Months, Utc};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use std::error::Error;
use std::thread;
use std::time::Duration;

/// Top-lists page loads player rows via AJAX from this endpoint.
const TOP_LIST_URL: &str = "https://ratings.fide.com/a_top.php?list=open";

/// Calculations page loads game rows via AJAX from this endpoint
/// (`calculations.phtml?id_number=…&period=…&rating=0`).
const CALC_URL: &str =
    "https://ratings.fide.com/a_indv_calculation.php?id_number={id}&rating_period={period}&t=0";

struct Player {
    fide_id: String,
    name: String,
    rating: String,
}

struct Game {
    color: String,
    opponent_rating: String,
    result: String,
}

fn main() -> Result<(), Box<dyn Error>> {
    let client = build_client()?;
    let players = fetch_players(&client)?;
    let periods = last_12_complete_months();

    for player in &players {
        println!("{}\t{}\t{}", player.name, player.fide_id, player.rating);

        for period in &periods {
            thread::sleep(Duration::from_millis(200));
            let html = fetch_calculations(&client, &player.fide_id, period)?;
            for game in parse_games(&html)? {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    player.name,
                    player.fide_id,
                    period,
                    game.color,
                    game.opponent_rating,
                    game.result
                );
            }
        }
    }

    Ok(())
}

fn build_client() -> Result<Client, Box<dyn Error>> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("alt-rating/0.1"));
    headers.insert(
        "X-Requested-With",
        HeaderValue::from_static("XMLHttpRequest"),
    );

    Ok(Client::builder().default_headers(headers).build()?)
}

fn fetch_players(client: &Client) -> Result<Vec<Player>, Box<dyn Error>> {
    let html = client.get(TOP_LIST_URL).send()?.error_for_status()?.text()?;
    let re = Regex::new(
        r#"(?s)<a href=/profile/(\d+)>([^<]+)</a>.*?<td class=rating_column>(\d+)</td>"#,
    )?;

    Ok(re
        .captures_iter(&html)
        .map(|caps| Player {
            fide_id: caps[1].to_string(),
            name: caps[2].to_string(),
            rating: caps[3].to_string(),
        })
        .collect())
}

fn last_12_complete_months() -> Vec<String> {
    let today = Utc::now().date_naive();
    let first_of_this_month =
        chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap();

    (0..12)
        .rev()
        .map(|i| {
            let d = first_of_this_month - Months::new(i + 1);
            format!("{:04}-{:02}-01", d.year(), d.month())
        })
        .collect()
}

fn fetch_calculations(
    client: &Client,
    fide_id: &str,
    period: &str,
) -> Result<String, Box<dyn Error>> {
    let url = CALC_URL
        .replacen("{id}", fide_id, 1)
        .replacen("{period}", period, 1);
    Ok(client.get(url).send()?.error_for_status()?.text()?)
}

fn parse_games(html: &str) -> Result<Vec<Game>, Box<dyn Error>> {
    // Game rows: <span class="white_note|black_note"> … opponent rating … result (w)
    let re = Regex::new(
        r#"(?s)<span class="(white|black)_note">.*?</span>.*?<td\s+class="list4">(\d+)\s*</td>\s*<td\s+class="list4 table_scale">[A-Z]{3}</td>\s*<td\s+class=list4>(\d+\.\d+)</td>"#,
    )?;

    Ok(re
        .captures_iter(html)
        .map(|caps| Game {
            color: caps[1].to_string(),
            opponent_rating: caps[2].to_string(),
            result: caps[3].to_string(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_color_rating_result() {
        let html = r#"
			<tr bgcolor=#efefef>
				<td class=list4 style="display: flex;"><span class="black_note">&nbsp;</span> Firouzja, Alireza</td>
				<td  class="list4">g</td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2759 </td>
				<td  class="list4 table_scale">FRA</td>
				<td  class=list4>0.00</td>
				<td  class="list4">1</td>
			</tr>
			<tr bgcolor=#efefef>
				<td class=list4><span class="white_note">&nbsp;</span> Keymer, Vincent</td>
				<td  class="list4">g</td>
				<td  class="list4 table_scale"></td>
				<td  class="list4">2759 </td>
				<td  class="list4 table_scale">GER</td>
				<td  class=list4>0.50</td>
				<td  class="list4">1</td>
			</tr>"#;
        let games = parse_games(html).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].color, "black");
        assert_eq!(games[0].opponent_rating, "2759");
        assert_eq!(games[0].result, "0.00");
        assert_eq!(games[1].color, "white");
        assert_eq!(games[1].result, "0.50");
    }

    #[test]
    fn last_twelve_complete_months_are_ordered() {
        let periods = last_12_complete_months();
        assert_eq!(periods.len(), 12);
        assert!(periods[0].ends_with("-01"));
        assert_eq!(&periods[0][8..], "01");
        // contiguous months ending with previous calendar month
        assert!(periods[0] < periods[11]);
    }
}
