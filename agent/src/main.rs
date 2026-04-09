pub mod sender;
pub mod retry;

use axum::{
    routing::{get, post},
    Json, Router, extract::State,
};
use serde_json::{json, Value};
use shared::{db::init_db, generate_merkle_root, AuditLog};
use std::sync::Arc;
use tokio::sync::mpsc;
use std::time::Duration;
use tower_http::services::ServeDir;
use dotenv::dotenv;

use crate::retry::{FailedBatch, spawn_retry_worker};
use crate::sender::send_merkle_root_to_solana;

#[derive(Clone)]
struct AppState {
    tx: mpsc::Sender<AuditLog>,
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt::init();
    
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "verilog.db".to_string());
    let dashboard_path = std::env::var("DASHBOARD_PATH").unwrap_or_else(|_| "../dashboard".to_string());

    // Setup MPSC queue for Logs
    let (tx, mut rx) = mpsc::channel::<AuditLog>(10000);
    // Setup MPSC queue for Retries
    let (retry_tx, retry_rx) = mpsc::channel::<FailedBatch>(1000);
    
    spawn_retry_worker(retry_rx);
    
    // Spawn Batcher Task
    let retry_tx_clone = retry_tx.clone();
    let db_path_clone = db_path.clone();
    
    tokio::spawn(async move {
        let conn = init_db(&db_path_clone).expect("Failed to bind DB");
        let mut batch_cache = Vec::new();

        loop {
            if let Ok(Some(log)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
                batch_cache.push(log);
            }

            if batch_cache.len() >= 100 || (!batch_cache.is_empty() && batch_cache.len() < 100) {
                let batch_id = format!("batch_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis());
                let merkle_root = generate_merkle_root(&batch_cache);
                let service_id = batch_cache[0].service.clone();
                
                println!("[Batcher] Root Generated: {}", merkle_root);
                
                conn.execute(
                    "INSERT INTO batches (id, merkle_root, service_id) VALUES (?1, ?2, ?3)",
                    rusqlite::params![batch_id, merkle_root, service_id],
                ).unwrap();

                for item in &batch_cache {
                    conn.execute(
                        "INSERT INTO audit_logs (service, user_id, amount, timestamp, hash, batch_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![item.service, item.user_id, item.amount, item.timestamp, item.hash, batch_id],
                    ).unwrap();
                }
                
                println!("[Batcher] {} logs saved off-chain. Menghubungi Solana...", batch_cache.len());
                let solana_success = send_merkle_root_to_solana(&batch_id, &merkle_root, &service_id).await;
                
                if !solana_success {
                    println!("[Batcher] RPC Gagal. Pushing to Retry Queue...");
                    let _ = retry_tx_clone.send(FailedBatch {
                        batch_id: batch_id.clone(),
                        merkle_root,
                        service_id,
                    }).await;
                }
                
                batch_cache.clear();
            }
        }
    });

    let state = AppState { tx };

    let app = Router::new()
        .route("/api/collect", post(collect_log))
        .route("/api/verify", get(verify_batch))
        .nest_service("/", ServeDir::new(&dashboard_path))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Agent Server running on port 3000 (DB: {}, Dashboard: {})", db_path, dashboard_path);
    axum::serve(listener, app).await.unwrap();
}

async fn collect_log(
    State(state): State<AppState>,
    Json(mut payload): Json<AuditLog>,
) -> Json<Value> {
    payload.hash = Some(payload.compute_hash());
    if state.tx.try_send(payload).is_ok() {
        Json(json!({ "success": true }))
    } else {
        Json(json!({ "success": false, "error": "Queue full" }))
    }
}

async fn verify_batch() -> Json<Value> {
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "verilog.db".to_string());
    let conn = init_db(&db_path).unwrap();
    let result: rusqlite::Result<(String, String)> = conn.query_row(
        "SELECT id, merkle_root FROM batches ORDER BY created_at DESC LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    );

    if let Ok((batch_id, stored_root)) = result {
        let mut stmt = conn.prepare("SELECT service, user_id, amount, timestamp, hash FROM audit_logs WHERE batch_id = ?1").unwrap();
        let logs_iter = stmt.query_map([batch_id], |row| {
            Ok(AuditLog {
                id: None,
                service: row.get(0)?,
                user_id: row.get(1)?,
                amount: row.get(2)?,
                timestamp: row.get(3)?,
                hash: Some(row.get(4)?),
            })
        }).unwrap();
        
        let mut logs = Vec::new();
        for log in logs_iter { logs.push(log.unwrap()); }
        
        let dynamic_root = generate_merkle_root(&logs);
        if dynamic_root == stored_root {
            Json(json!({ "status": "VALID", "detail": "Merkle Hash Matching via rs-merkle" }))
        } else {
            Json(json!({ "status": "TAMPERED", "detail": format!("Mismatch!\nOff-chain Recompute:\n{}\n\nOn-chain Mock:\n{}", dynamic_root, stored_root) }))
        }
    } else {
         Json(json!({ "status": "NO_DATA", "detail": "empty" }))
    }
}
