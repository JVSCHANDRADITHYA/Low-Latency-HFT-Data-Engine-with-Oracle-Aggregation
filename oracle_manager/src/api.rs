
use axum::{routing::get, Router};
use rand::Rng;
use std::net::SocketAddr;

// Mock oracle functions
fn fetch_pyth_price() -> f64 {
    100.0 + rand::rng().random_range(-1.0..1.0)
}

fn fetch_switchboard_price() -> f64 {
    100.0 + rand::rng().random_range(-1.5..1.5)
}

// /price endpoint
async fn price_handler() -> String {
    let p = fetch_pyth_price();
    let s = fetch_switchboard_price();
    let consensus = (p + s) / 2.0;

    format!(
        "{{\"pyth\": {}, \"switchboard\": {}, \"consensus\": {}}}",
        p, s, consensus
    )
}

// /health endpoint
async fn health_handler() -> &'static str {
    "{\"status\": \"ok\"}"
}

pub async fn start_server() {
    let app = Router::new()
        .route("/price", get(price_handler))
        .route("/health", get(health_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    println!("Running server at http://{}", addr);

    axum::serve(tokio::net::TcpListener::bind(addr).await.unwrap(), app)
        .await
        .unwrap();
}
