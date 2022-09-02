use crate::pagerduty::FinalPagerDutySchedule;
use crate::webserver::{start_webserver, Callback};
use chrono::{DateTime, Duration, FixedOffset, NaiveDateTime};
use oauth2::basic::BasicClient;
use oauth2::reqwest::async_http_client;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RedirectUrl,
    Scope, TokenResponse, TokenUrl,
};
use reqwest::Url;
use reqwest::{self, Client};
use serde::Deserialize;
use serde_json;
use std::process::Command;
use tokio::sync::mpsc::{channel, Receiver, Sender};

#[derive(Deserialize, Debug)]
struct CalendarEventResponse {
    items: Vec<CalendarEvent>,
}

#[derive(Deserialize, Debug)]
pub struct CalendarEvent {
    pub visibility: Option<String>,
    pub summary: Option<String>,
    // creator: Option<EventCreator>,
    pub start: Option<TimeWrapper>,
    pub end: Option<TimeWrapper>,
    #[serde(rename = "eventType")]
    pub event_type: Option<String>,
    // extra metadata after joining
    pub pagerduty: Option<FinalPagerDutySchedule>,
}

#[derive(Deserialize, Debug)]
pub struct TimeWrapper {
    #[serde(rename = "date")]
    pub date_string: Option<String>,
    #[serde(rename = "dateTime")]
    pub date_time_string: Option<String>,
    // #[serde(rename = "timeZone")]
    // timezone: Option<String>,
}

pub fn get_start_end_time(
    start_date: &str,
    duration_days: i64,
) -> (DateTime<FixedOffset>, DateTime<FixedOffset>) {
    let sgt_timezone = FixedOffset::east(8 * 60 * 60);
    let start_time =
        NaiveDateTime::parse_from_str(&format!("{} 00:00", start_date), "%Y-%m-%d %H:%M").unwrap();
    let start_time_local = DateTime::<FixedOffset>::from_local(start_time, sgt_timezone);

    let end_time_local = start_time_local
        .checked_add_signed(Duration::days(duration_days))
        .unwrap();

    return (start_time_local, end_time_local);
}

pub async fn check_token_validity(client: &Client, token: &str) -> Result<(), String> {
    let url = "https://www.googleapis.com/calendar/v3/users/me/calendarList";
    let request = client
        .get(url)
        .header("Authorization", format!("Bearer {}", token));

    let response = request.send().await;

    match response {
        Ok(inside) if inside.status() == 401 => Err("Unauthorised".to_string()),
        Ok(_) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

pub async fn get_user_calender(
    client: &Client,
    pd_user: FinalPagerDutySchedule,
    token: &str,
    start_time_local: DateTime<FixedOffset>,
    end_time_local: DateTime<FixedOffset>,
) -> (FinalPagerDutySchedule, Vec<CalendarEvent>) {
    let event_url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/{}/events",
        pd_user.email
    );

    let params = vec![
        ("timeMin", start_time_local.to_rfc3339()),
        ("timeMax", end_time_local.to_rfc3339()),
        ("timeZone", "Asia/Singapore".to_string()),
    ];
    let url = Url::parse_with_params(&event_url, params).unwrap();

    let request = client
        .get(url)
        .header("Authorization", format!("Bearer {}", token));

    let response = request.send().await;

    let response_parsed = match response {
        Ok(inside) if inside.status() != 200 => {
            panic!("Non 200 response. Panicking {}", &inside.status())
        }
        Ok(inside) => inside.text().await,
        Err(e) => panic!("Panicking because of error {}", e.to_string()),
    };

    let result = response_parsed.expect("Expected valid text");
    // println!("{}", &result);

    let parsed: CalendarEventResponse = serde_json::from_str(&result).unwrap();
    let public_events = parsed.items.into_iter().filter(|x| match &x.visibility {
        Some(v) if v != "private" => true,
        _ => false,
    });

    let xoncall_calendar_events: Vec<CalendarEvent> = public_events
        .filter(should_not_be_oncall)
        .map(|mut x| {
            x.pagerduty = Some(pd_user.clone());
            x
        })
        .collect();
    return (pd_user, xoncall_calendar_events);
}

fn should_not_be_oncall(event: &CalendarEvent) -> bool {
    match &event.summary {
        Some(value) if value.to_lowercase().contains("xoncall") => true,
        Some(value) if value.to_lowercase().contains("out of") => true,
        Some(_) if event.event_type.is_some() => match &event.event_type {
            Some(event_type) if event_type.to_lowercase() == "outofoffice" => true,
            _ => false,
        },
        // Some(value) if value.to_lowercase().contains("ooo") => true,
        _ => false,
    }
}

pub async fn get_oauth_token(client_id: &str, secret: &str) -> Result<String, String> {
    let auth_url = "https://accounts.google.com/o/oauth2/auth".to_string();
    let token_url = "https://oauth2.googleapis.com/token".to_string();
    // let redirect_url = "urn:ietf:wg:oauth:2.0:oob".to_string();
    let redirect_url = "http://localhost:8080/oauth_callback".to_string();

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let oidcclient = BasicClient::new(
        ClientId::new(client_id.to_string()),
        Some(ClientSecret::new(secret.to_string())),
        AuthUrl::new(auth_url).unwrap(),
        Some(TokenUrl::new(token_url).unwrap()),
    )
    // Set the URL the user will be redirected to after the authorization process.
    .set_redirect_uri(RedirectUrl::new(redirect_url).unwrap());

    let (auth_url, _csrf_token) = oidcclient
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(
            "https://www.googleapis.com/auth/calendar.readonly".to_string(),
        ))
        .set_pkce_challenge(pkce_challenge)
        .url();

    // Start a webserver with a channel to receive the authorisation code
    let (sender, mut receiver): (Sender<Callback>, Receiver<Callback>) = channel(1);

    let webserver_to_start = start_webserver(sender);
    let mut handle = tokio::spawn(webserver_to_start.await);

    println!("Attempting to open oauth url with browser: {}", auth_url);
    let _ = Command::new("open")
        .arg(auth_url.to_string())
        .output()
        .expect("Failed to open url with browswer");

    tokio::select! {
        _ = &mut handle =>  {return Err("Not ok".to_string())}
        // x = server => {return Err(format!("Web server unexpectedly exited with reason: {:?}", x))}

        message = receiver.recv() => {
            let retrieved_callback = message.expect("Expected value from channel, but channel ws closed");
            // TODO: Close server
            handle.abort();
            let token = oidcclient
            .exchange_code(AuthorizationCode::new(retrieved_callback.code))
            // Set the PKCE code verifier.
            .set_pkce_verifier(pkce_verifier)
            .request_async(async_http_client)
            .await
            .unwrap()
            .access_token()
            .secret()
            .clone();
            return Ok(token)
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_not_be_oncall() {
        let ooo = CalendarEvent {
            visibility: Some("public".to_string()),
            summary: Some("Out of Office".to_string()),
            start: None,
            end: None,
            pagerduty: None,
            event_type: None,
        };
        assert_eq!(should_not_be_oncall(&ooo), true);
        let xoncall = CalendarEvent {
            visibility: Some("public".to_string()),
            summary: Some("xoncall".to_string()),
            start: None,
            end: None,
            pagerduty: None,
            event_type: None,
        };
        assert_eq!(should_not_be_oncall(&xoncall), true);
    }
}
