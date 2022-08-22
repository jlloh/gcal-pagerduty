use chrono::{DateTime, FixedOffset};
use futures::future::join_all;
use reqwest::Url;
use reqwest::{self, Client};
use serde::Deserialize;
use serde_json;

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
) -> Vec<FinalPagerDutySchedule> {
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
    let url = Url::parse_with_params(&url_base, params).unwrap();

    let request = client
        .get(url)
        .header("Authorization", format!("Token token={}", api_key));
    // println!("{:?}", &request);

    let response = request.send().await;

    let response_text = match response {
        Ok(inside) if inside.status() != 200 => {
            panic!(
                "Non 200 response for pd_api. Panicking {}",
                &inside.status()
            )
        }
        Ok(inside) => inside.text().await,
        Err(e) => panic!("Panicking for pd_api because of error {}", e.to_string()),
    };

    let schedule: ScheduleResponse = match response_text {
        Ok(inside) => match serde_json::from_str(&inside) {
            Ok(parsed) => parsed,
            Err(e) => panic!(
                "Failed to parse  from json with error: {}",
                // inside,
                e.to_string()
            ),
        },
        Err(e) => panic!("Panicking for pd_api because of error {}", e.to_string()),
    };
    //: ScheduleResponse
    // &response.unwrap().text().await.unwrap()).unwrap();

    // retrieve emails of usrs
    let scheduled_entries = schedule.schedule.final_schedule.rendered_schedule_entries;
    let futures = scheduled_entries
        .into_iter()
        .map(|entry| get_pd_user_email(client, &api_key, entry));

    let results = join_all(futures).await;
    return results;
}

async fn get_pd_user_email(
    client: &Client,
    api_key: &str,
    entry: ScheduleEntry,
) -> FinalPagerDutySchedule {
    let endpoint = match entry.user.api_url {
        Some(value) => value,
        None => panic!(
            "Possible invalid user in pagerduty: {:?}",
            entry.user.summary
        ),
    };
    let request = client
        .get(endpoint)
        .header("Authorization", format!("Token token={}", api_key));
    // println!("{:?}", &request);

    let response_text = request.send().await.unwrap().text().await.unwrap();
    // println!("{}", response_text);

    let user_response: PagerDutyUserResponse = serde_json::from_str(&response_text).unwrap();

    let start_time = DateTime::<FixedOffset>::parse_from_rfc3339(&entry.start).unwrap();
    let end_time = DateTime::<FixedOffset>::parse_from_rfc3339(&entry.end).unwrap();

    return FinalPagerDutySchedule {
        start: start_time,
        end: end_time,
        email: user_response.user.email,
    };
}
