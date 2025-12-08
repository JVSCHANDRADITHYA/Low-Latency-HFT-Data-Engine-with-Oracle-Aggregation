use axum::{routing::get, Json, Router};
use redis::Commands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

// ✅ PROMETHEUS IMPORTS
use prometheus::{
    Encoder, TextEncoder, IntCounter, Histogram,
    register_int_counter, register_histogram,
};
use std::sync::Once;

// ============================
// ✅ PROMETHEUS GLOBAL METRICS
// ============================

static INIT: Once = Once::new();

static mut BTC_FETCH_COUNT: Option<IntCounter> = None;
static mut API_HIT_COUNT: Option<IntCounter> = None;
static mut FETCH_LATENCY: Option<Histogram> = None;

fn init_metrics() {
    unsafe {
        INIT.call_once(|| {
            BTC_FETCH_COUNT = Some(register_int_counter!(
                "btc_price_fetch_total",
                "Total BTC price fetches"
            ).unwrap());

            API_HIT_COUNT = Some(register_int_counter!(
                "btc_api_hits_total",
                "Total API calls to BTC endpoint"
            ).unwrap());

            FETCH_LATENCY = Some(register_histogram!(
                "btc_fetch_latency_seconds",
                "Latency of BTC price fetch"
            ).unwrap());
        });
    }
}

// ============================
// ✅ DATA STRUCTURES
// ============================

#[derive(Serialize)]
struct PriceResponse {
    symbol: String,
    price: f64,
    confidence: f64,
    timestamp: i64,
}

// ✅ Pyth v2 response structs
#[derive(Deserialize, Debug)]
struct PythLatest {
    parsed: Vec<PythParsed>,
}

#[derive(Deserialize, Debug)]
struct PythParsed {
    price: PythInnerPrice,
}

#[derive(Deserialize, Debug)]
struct PythInnerPrice {
    // Pyth v2 sends these as STRINGS
    price: String,
    conf: String,
    expo: i32,
}

// ============================
// ✅ MAIN ENTRY
// ============================

#[tokio::main]
async fn main() {
    // ✅ INIT PROMETHEUS METRICS
    init_metrics();

    // ✅ POSTGRES CONNECTION
    let db = PgPool::connect("postgres://oracle_user:oracle_pass@localhost/oracle")
        .await
        .unwrap();

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS price_history (
            id SERIAL PRIMARY KEY,
            symbol TEXT,
            timestamp BIGINT,
            price DOUBLE PRECISION,
            confidence DOUBLE PRECISION
        )",
    )
    .execute(&db)
    .await
    .unwrap();

    println!("✅ Oracle Manager Started");

    let db_clone = db.clone();

    // ============================
    // ✅ BACKGROUND PRICE FETCHER
    // ============================

    tokio::spawn(async move {
        let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();
        let http = reqwest::Client::new();

        // Pyth v2 BTC/USD feed ID (you fetched this with jq)
        const BTC_FEED_ID: &str =
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";

        loop {
            let timer = unsafe {
                FETCH_LATENCY.as_ref().unwrap().start_timer()
            };

            let res = http
                .get(&format!(
                    "https://hermes.pyth.network/v2/updates/price/latest?ids[]={}",
                    BTC_FEED_ID
                ))
                .send()
                .await;

            timer.observe_duration();

            unsafe {
                BTC_FETCH_COUNT.as_ref().unwrap().inc();
            }

            if let Ok(resp) = res {
                if let Ok(json) = resp.json::<PythLatest>().await {
                    if json.parsed.is_empty() {
                        eprintln!("Pyth response parsed but empty");
                    } else {
                        let price_data = &json.parsed[0].price;

                        // Convert string -> f64
                        let raw_price: f64 = price_data.price.parse().unwrap();
                        let raw_conf: f64 = price_data.conf.parse().unwrap();

                        // Apply exponent
                        let price =
                            raw_price * 10f64.powi(price_data.expo);
                        let confidence =
                            raw_conf * 10f64.powi(price_data.expo);

                        let ts = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs() as i64;

                        let payload = PriceResponse {
                            symbol: "BTC".into(),
                            price,
                            confidence,
                            timestamp: ts,
                        };

                        let json_str = serde_json::to_string(&payload).unwrap();

                        let mut redis = redis_client.get_connection().unwrap();
                        let _: () = redis.set("oracle:price:BTC", json_str).unwrap();

                        sqlx::query(
                            "INSERT INTO price_history (symbol, timestamp, price, confidence)
                             VALUES ($1,$2,$3,$4)",
                        )
                        .bind("BTC")
                        .bind(ts)
                        .bind(price)
                        .bind(confidence)
                        .execute(&db_clone)
                        .await
                        .unwrap();

                        println!("BTC ${:.2}", price);
                    }
                } else {
                    eprintln!("Failed to parse Pyth JSON");
                }
            } else {
                eprintln!("HTTP request to Pyth failed");
            }

            tokio::time::sleep(std::time::Duration::from_nanos(1)).await;
        }
    });
    
    // ============================
    // ✅ AXUM ROUTES
    // ============================

    let app = Router::new()
        .route("/oracle/price/BTC", get(get_btc_price))
        .route("/metrics", get(metrics_handler));

    println!("✅ REST API Running at http://localhost:3000/oracle/price/BTC");
    println!("✅ Prometheus Metrics at http://localhost:3000/metrics");

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ============================
// ✅ BTC API HANDLER
// ============================

async fn get_btc_price() -> Json<serde_json::Value> {
    unsafe {
        API_HIT_COUNT.as_ref().unwrap().inc();
    }

    let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();

    let mut redis = match redis_client.get_connection() {
        Ok(c) => c,
        Err(_) => {
            return Json(serde_json::json!({
                "status": "error",
                "message": "Redis connection failed"
            }));
        }
    };

    let val: Result<String, _> = redis.get("oracle:price:BTC");

    match val {
        Ok(v) => Json(serde_json::from_str(&v).unwrap()),
        Err(_) => Json(serde_json::json!({
            "status": "warming_up",
            "message": "BTC price not cached yet"
        })),
    }
}

// ============================
// ✅ PROMETHEUS METRICS ENDPOINT
// ============================

async fn metrics_handler() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    let mf = prometheus::gather();
    encoder.encode(&mf, &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
