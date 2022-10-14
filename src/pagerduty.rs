use anyhow::{anyhow, Context, Result as AnyhowResult};
use chrono::{DateTime, FixedOffset};
use futures::future::join_all;
use reqwest::Url;
use reqwest::{self, Client};
use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct ScheduleResponse {
    schedule: Schedule,
}

#[derive(Deserialize, Debug)]
struct Schedule {
    final_schedule: FinalSchedule,
}

#[derive(Deserialize, Debug)]
struct PagerDutyUser {
    summary: String,
    #[serde(rename = "self")]
    api_url: Option<String>,
}

#[derive(Deserialize, Debug)]
struct PagerDutyUserResponse {
    user: PagerDutyUserMetadata,
}

#[derive(Deserialize, Debug)]
struct PagerDutyUserMetadata {
    email: String,
}

#[derive(Deserialize, Debug)]
struct FinalSchedule {
    rendered_schedule_entries: Vec<ScheduleEntry>,
}

#[derive(Deserialize, Debug)]
struct ScheduleEntry {
    start: String,
    end: String,
    user: PagerDutyUser,
}

#[derive(Deserialize, Debug, Clone)]
pub struct FinalPagerDutySchedule {
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
    pub email: String,
}

pub async fn get_pagerduty_schedule(
    client: &Client,
    api_key: String,
    schedule_id: String,
    start_time_local: DateTime<FixedOffset>,
    end_time_local: DateTime<FixedOffset>,
) -> AnyhowResult<Vec<FinalPagerDutySchedule>> {
    let url_base = format!("https://api.pagerduty.com/schedules/{}", schedule_id);
    println!(
        "Retrieving pd schedule from {} to {}",
        &start_time_local, &end_time_local
    );
    let params = vec![
        ("since", start_time_local.to_rfc3339()),
        ("until", end_time_local.to_rfc3339()),
        ("time_zone", "Asia/Singapore".to_string()),
    ];
    let url = Url::parse_with_params(&url_base, params).context("Failed to parse url")?;

    let request = client
        .get(url)
        .header("Authorization", format!("Token token={}", api_key));

    let response_text = request
        .send()
        .await
        .context("Failed to call pd api")?
        .text()
        .await;

    let schedule: ScheduleResponse = serde_json::from_str(
        &response_text.context("Failed to get text response from pd api call")?,
    )
    .context("Failed to parse json from pd api response")?;

    // retrieve emails of usrs
    let scheduled_entries = schedule.schedule.final_schedule.rendered_schedule_entries;
    let futures = scheduled_entries
        .into_iter()
        .map(|entry| get_pd_user_email(client, &api_key, entry));

    let results = join_all(futures).await;

    let results_filtered = results
        .into_iter()
        .filter(|result| match result {
            Ok(_) => true,
            Err(e) => {
                println!("Warning. Pd lookup failed with error: {}. Skipping.", e);
                false
            }
        })
        .flatten()
        .collect();

    Ok(results_filtered)
}

async fn get_pd_user_email(
    client: &Client,
    api_key: &str,
    entry: ScheduleEntry,
) -> AnyhowResult<FinalPagerDutySchedule> {
    let endpoint = match entry.user.api_url {
        Some(value) => value,
        None => {
            return Err(anyhow!(
                "Possible invalid user in pagerduty: {}",
                entry.user.summary
            ))
        }
    };
    let request = client
        .get(endpoint)
        .header("Authorization", format!("Token token={}", api_key));

    let response_text = request
        .send()
        .await
        .context("Failed to call pd api to get user email")?
        .text()
        .await
        .context("Failed to convert pd api response to text")?;

    let user_response: PagerDutyUserResponse = serde_json::from_str(&response_text)
        .context("Failed to parse pagerdutyuserresponse as json")?;

    let start_time = DateTime::<FixedOffset>::parse_from_rfc3339(&entry.start)
        .context("Failed to parse start_time as rfc3339")?;
    let end_time = DateTime::<FixedOffset>::parse_from_rfc3339(&entry.end)
        .context("Failed to parse end_time as rfc3339")?;

    Ok(FinalPagerDutySchedule {
        start: start_time,
        end: end_time,
        email: user_response.user.email,
    })
}
