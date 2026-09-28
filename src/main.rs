use regex::Regex;
use reqwest::blocking::Client;
use std::error::Error;

/// The top-lists page loads player rows via AJAX from this endpoint.
const TOP_LIST_URL: &str = "https://ratings.fide.com/a_top.php?list=open";

fn main() -> Result<(), Box<dyn Error>> {
    let html = Client::new()
        .get(TOP_LIST_URL)
        .header(
            reqwest::header::USER_AGENT,
            "alt-rating/0.1 (educational; contact: local)",
        )
        .send()?
        .error_for_status()?
        .text()?;

    let re = Regex::new(
        r#"(?s)<a href=/profile/(\d+)>([^<]+)</a>.*?<td class=rating_column>(\d+)</td>"#,
    )?;

    for caps in re.captures_iter(&html) {
        let fide_id = &caps[1];
        let name = &caps[2];
        let rating = &caps[3];
        println!("{name}\t{fide_id}\t{rating}");
    }

    Ok(())
}
