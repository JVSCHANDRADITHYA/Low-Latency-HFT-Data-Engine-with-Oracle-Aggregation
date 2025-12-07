use axum::{routing::get, Json, Router};
use redis::Commands;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;

#[derive(Serialize)]
struct PriceResponse {
    symbol: String,
    price: f64,
    confidence: f64,
    timestamp: i64,
}

#[derive(Deserialize, Debug)]
struct PythPrice {
    price: i64,
    conf: i64,
    expo: i32,
}

#[derive(Deserialize, Debug)]
struct PythFeed {
    price: PythPrice,
}

#[tokio::main]
async fn main() {
    // ✅ PostgreSQL
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

    // ✅ BACKGROUND PRICE FETCH (PYTH HTTP ORACLE)
    tokio::spawn(async move {
        let redis_client = redis::Client::open("redis://127.0.0.1/").unwrap();
        let http = reqwest::Client::new();

        loop {
            let res = http
                .get("https://hermes.pyth.network/api/latest_price_feeds?ids[]=BTC/USD")
                .send()
                .await;

            if let Ok(resp) = res {
                if let Ok(json) = resp.json::<Vec<PythFeed>>().await {
                    let price_data = &json[0].price;

                    let price = price_data.price as f64 * 10f64.powi(price_data.expo);
                    let confidence =
                        price_data.conf as f64 * 10f64.powi(price_data.expo);

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
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    // ✅ REST API
    let app = Router::new().route("/oracle/price/BTC", get(get_btc_price));

    println!("✅ REST API Running at http://localhost:3000/oracle/price/BTC");

    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn get_btc_price() -> Json<serde_json::Value> {
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
