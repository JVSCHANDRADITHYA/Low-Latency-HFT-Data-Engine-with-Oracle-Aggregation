use axum::{routing::get, Json, Router};
use prometheus::{
    Encoder, Histogram, IntCounter, TextEncoder, register_histogram,
    register_int_counter,
};
use redis::Commands;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use std::{sync::Once, time::{SystemTime, UNIX_EPOCH}};
use tokio::net::TcpListener;
use serde_json::json;


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
                "btc_price_fetch_total", "Total BTC price fetches"
            ).unwrap());
            API_HIT_COUNT = Some(register_int_counter!(
                "btc_api_hits_total", "Total API hits"
            ).unwrap());
            FETCH_LATENCY = Some(register_histogram!(
                "btc_fetch_latency_seconds", "BTC Fetch Latency"
            ).unwrap());
        });
    }
}

// ============================
// ✅ DATA STRUCTURES
// ============================

#[derive(Serialize, Deserialize)]
struct PriceResponse {
    symbol: String,
    source: String,
    price: f64,
    confidence: f64,
    timestamp: i64,
}

#[derive(Deserialize)]
struct PythLatest {
    parsed: Vec<PythParsed>,
}

#[derive(Deserialize)]
struct PythParsed {
    price: PythInnerPrice,
}

#[derive(Deserialize)]
struct PythInnerPrice {
    price: String,
    conf: String,
    expo: i32,
}

// ============================
// ✅ MAIN ENTRY
// ============================

#[tokio::main]
async fn main() {
    init_metrics();

    let db = PgPool::connect("postgres://oracle_user:oracle_pass@localhost/oracle")
        .await
        .unwrap();

    // ✅ Create base table if it doesn't exist (OLD schema safe)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS price_history (
            id SERIAL PRIMARY KEY,
            symbol TEXT NOT NULL,
            timestamp BIGINT NOT NULL,
            price DOUBLE PRECISION NOT NULL,
            confidence DOUBLE PRECISION NOT NULL
        )"
    )
    .execute(&db)
    .await
    .unwrap();

    // ✅ Auto-add `source` column if missing (NO MANUAL SQL NEEDED)
    let column_exists: Option<(String,)> = sqlx::query_as(
        "SELECT column_name FROM information_schema.columns 
        WHERE table_name='price_history' AND column_name='source'"
    )
    .fetch_optional(&db)
    .await
    .unwrap();

    if column_exists.is_none() {
        println!("⚠️  Migrating DB: Adding source column...");
        sqlx::query("ALTER TABLE price_history ADD COLUMN source TEXT DEFAULT 'Pyth'")
            .execute(&db)
            .await
            .unwrap();
        println!("✅ Migration completed");
    }


    println!("✅ Oracle Manager Started");

    let db_clone = db.clone();

    tokio::spawn(async move {
        let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();
        let http = reqwest::Client::new();

        const BTC_FEED_ID: &str =
            "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";

        loop {
            let timer = unsafe { FETCH_LATENCY.as_ref().unwrap().start_timer() };

            let res = http
                .get(&format!(
                    "https://hermes.pyth.network/v2/updates/price/latest?ids[]={}",
                    BTC_FEED_ID
                ))
                .send()
                .await;

            unsafe { BTC_FETCH_COUNT.as_ref().unwrap().inc() };
            timer.observe_duration();

            if let Ok(resp) = res {
                if let Ok(json) = resp.json::<PythLatest>().await {
                    let p = &json.parsed[0].price;

                    let price = p.price.parse::<f64>().unwrap() * 10f64.powi(p.expo);
                    let confidence = p.conf.parse::<f64>().unwrap() * 10f64.powi(p.expo);
                    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;

                    let payload = PriceResponse {
                        symbol: "BTC".into(),
                        source: "Pyth".into(),
                        price,
                        confidence,
                        timestamp: ts,
                    };

                    let mut redis = redis_client.get_connection().unwrap();
                    let _: () = redis.set("oracle:price:BTC", serde_json::to_string(&payload).unwrap()).unwrap();


                    sqlx::query(
                        "INSERT INTO price_history (symbol, source, timestamp, price, confidence)
                         VALUES ($1,$2,$3,$4,$5)"
                    )
                    .bind("BTC")
                    .bind("Pyth")
                    .bind(ts)
                    .bind(price)
                    .bind(confidence)
                    .execute(&db_clone)
                    .await
                    .unwrap();

                    println!("✅ BTC ${:.2}", price);
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        }
    });

    let app = Router::new()
        .route("/oracle/price/BTC", get(get_btc_price))
        .route("/oracle/prices", get(get_all_prices))
        .route("/oracle/history/BTC", get(get_btc_history))
        .route("/oracle/health", get(get_oracle_health))
        .route("/metrics", get(metrics_handler));

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// ============================
// ✅ API HANDLERS
// ============================

async fn get_btc_price() -> Json<serde_json::Value> {
    unsafe { API_HIT_COUNT.as_ref().unwrap().inc() };
    let mut redis = redis::Client::open("redis://127.0.0.1/").unwrap().get_connection().unwrap();
    let val: Result<String, _> = redis.get("oracle:price:BTC");
    Json(val.ok().and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_else(|| json!({"status":"warming_up"})))
}

async fn get_all_prices() -> Json<serde_json::Value> {
    let mut redis = redis::Client::open("redis://127.0.0.1/").unwrap().get_connection().unwrap();
    let val: Result<String, _> = redis.get("oracle:price:BTC");
    Json(json!({ "BTC": val.ok() }))
}

async fn get_btc_history() -> Json<serde_json::Value> {
    let pool = PgPool::connect("postgres://oracle_user:oracle_pass@localhost/oracle").await.unwrap();
    let rows = sqlx::query("SELECT * FROM price_history WHERE symbol='BTC' ORDER BY timestamp DESC LIMIT 50")
        .fetch_all(&pool)
        .await
        .unwrap();

    Json(rows.iter().map(|r| json!({
        "timestamp": r.get::<i64,_>("timestamp"),
        "price": r.get::<f64,_>("price"),
        "confidence": r.get::<f64,_>("confidence"),
        "source": r.get::<String,_>("source")
    })).collect())
}

async fn get_oracle_health() -> Json<serde_json::Value> {
    let mut redis = redis::Client::open("redis://127.0.0.1/").unwrap().get_connection().unwrap();
    let val: Result<String, _> = redis.get("oracle:price:BTC");

    if let Ok(v) = val {
        if let Ok(p) = serde_json::from_str::<PriceResponse>(&v) {
            let age = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - p.timestamp;
            return Json(json!({
                "symbol": p.symbol,
                "status": if age < 30 { "healthy" } else { "stale" },
                "age": age
            }));
        }
    }

    Json(json!({ "status": "no_data" }))
}

// ============================
// ✅ METRICS
// ============================

async fn metrics_handler() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    encoder.encode(&prometheus::gather(), &mut buffer).unwrap();
    String::from_utf8(buffer).unwrap()
}
