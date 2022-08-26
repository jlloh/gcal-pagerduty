use actix_web::{
    get,
    web::{self, Data},
    App, HttpServer,
};
use serde::Deserialize;
use tokio::sync::mpsc::Sender;

pub struct AppState {
    pub sender_channel: Sender<Callback>,
}

// Have to use a channel to pass the response back to main thread
// oneshot channel?
pub async fn start_webserver(sender: Sender<Callback>) -> actix_web::dev::Server {
    println!("Starting local callback webserver");

    let server = HttpServer::new(move || {
        let app_state = Data::new(AppState {
            sender_channel: sender.clone(),
        });
        App::new().app_data(app_state).service(oauth_callback)
    });

    server.bind(("localhost", 8080)).unwrap().run()
}

#[derive(Deserialize)]
pub struct Callback {
    pub code: String,
}

#[get("/oauth_callback")]
async fn oauth_callback(req_body: web::Query<Callback>, app_state: web::Data<AppState>) -> String {
    let sender = &app_state.sender_channel;
    match sender.send(req_body.into_inner()).await {
        Ok(_) => return "Successfully exchanged auth data".to_string(),
        Err(e) => return format!("Channel was closed with error: {}", e.to_string()),
    }
}
