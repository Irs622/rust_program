pub mod sender;
pub mod retry;

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use serde::Deserialize;
use serde_json::{json, Value};
use shared::{
    db::init_db_from_connection, generate_merkle_root, ApiResponse, ApiErrorResponse, ApiErrorData, ApiMeta, AuditLog
};
use std::sync::Arc;
use tokio::sync::mpsc;
use std::time::{Duration, Instant};
use tower_http::services::ServeDir;
use dotenv::dotenv;

use crate::retry::{RpcJob, RpcResult};
use crate::sender::send_merkle_root_to_solana;

#[derive(Clone)]
struct AppConfig {
    env: String,
    db_path: String,
}

struct AppState {
    pool: Pool<SqliteConnectionManager>,
    config: AppConfig,
    rpc_tx: mpsc::Sender<RpcJob>,
    log_tx: mpsc::Sender<AuditLog>,
}

#[derive(Deserialize)]
struct LogQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[tokio::main]
async fn main() {
    dotenv().ok();
    tracing_subscriber::fmt::init();
    
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "verilog.db".to_string());
    let dashboard_path = std::env::var("DASHBOARD_PATH").unwrap_or_else(|_| "dashboard".to_string());
    let env = std::env::var("APP_ENV").unwrap_or_else(|_| "development".to_string());

    let config = AppConfig {
        env,
        db_path: db_path.clone(),
    };

    // Initialize Database and Pool
    let manager = SqliteConnectionManager::file(&db_path);
    let pool = Pool::new(manager).expect("Failed to create pool");
    
    {
        let conn = pool.get().expect("Failed to get conn for init");
        init_db_from_connection(&conn).expect("Failed to init DB");
    }

    // RPC Channels
    let (rpc_tx, mut rpc_rx) = mpsc::channel::<RpcJob>(1000);
    let (res_tx, mut res_rx) = mpsc::channel::<RpcResult>(1000);

    // 1. RPC Worker Thread (Sync/Blocking) - Option A
    let _pool_for_rpc = pool.clone();
    std::thread::spawn(move || {
        println!("[RPC Worker] Started.");
        while let Some(job) = rpc_rx.blocking_recv() {
            println!("[RPC Worker] Processing batch: {}", job.batch_id);
            let start = Instant::now();
            
            let result = send_merkle_root_to_solana(&job.batch_id, &job.merkle_root, &job.service_id);
            
            let (status, tx_hash, error) = match result {
                Ok(hash) => ("COMMITTED", Some(hash), None),
                Err(e) => {
                    println!("[RPC Worker] Error for {}: {}", job.batch_id, e);
                    ("FAILED", None, Some(e))
                }
            };
            
            let latency = start.elapsed().as_millis() as u64;
            println!("[RPC Worker] Finished {} in {}ms (Status: {})", job.batch_id, latency, status);

            let res = RpcResult {
                batch_id: job.batch_id,
                tx_hash,
                status: status.to_string(),
                error,
            };

            if let Err(e) = res_tx.blocking_send(res) {
                println!("[RPC Worker] Critical: Failed to send result back: {}", e);
            }
        }
    });

    // 2. Feedback Loop Task (Async)
    let pool_for_feedback = pool.clone();
    tokio::spawn(async move {
        while let Some(res) = res_rx.recv().await {
            let conn = pool_for_feedback.get().unwrap();
            
            // Enforce logic: only update to COMMITTED if currently PENDING
            let _ = conn.execute(
                "UPDATE batches SET chain_status = ?1, tx_hash = ?2 WHERE id = ?3 AND chain_status = 'PENDING'",
                rusqlite::params![res.status, res.tx_hash, res.batch_id],
            );
            
            if let Some(err) = res.error {
                let _ = conn.execute(
                    "INSERT INTO audit_logs (service, user_id, amount, timestamp, hash, event_type, actor_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params!["system", "rpc-worker", 0.0, chrono::Utc::now().to_rfc3339(), "", "ERROR", format!("Batch {} failed: {}", res.batch_id, err)],
                );
            }
        }
    });

    // 3. Batcher Task (Dedicated Thread)
    let pool_for_batcher = pool.clone();
    let rpc_tx_for_batcher = rpc_tx.clone();
    let (log_tx, mut log_rx) = mpsc::channel::<AuditLog>(10000);
    
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let mut batch_cache = Vec::new();
            loop {
                let log_res = tokio::time::timeout(Duration::from_secs(2), log_rx.recv()).await;
                let is_timeout = log_res.is_err();
                
                if let Ok(Some(log)) = log_res {
                    batch_cache.push(log);
                }

                if !batch_cache.is_empty() && (batch_cache.len() >= 50 || is_timeout) {
                     let conn = pool_for_batcher.get().unwrap();
                     let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis();
                     let batch_id = format!("batch_{}", timestamp);
                     let merkle_root = generate_merkle_root(&batch_cache);
                     let service_id = batch_cache[0].service.clone();

                     println!("[Batcher] Creating batch {} with {} logs", batch_id, batch_cache.len());

                     let _ = conn.execute(
                         "INSERT INTO batches (id, merkle_root, service_id, chain_status) VALUES (?1, ?2, ?3, 'PENDING')",
                         rusqlite::params![batch_id, merkle_root, service_id],
                     );

                     for item in &batch_cache {
                         let _ = conn.execute(
                             "INSERT INTO audit_logs (service, user_id, amount, timestamp, hash, batch_id, event_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'INGEST')",
                             rusqlite::params![item.service, item.user_id, item.amount, item.timestamp, item.hash, batch_id],
                         );
                     }
                     
                     let _ = rpc_tx_for_batcher.send(RpcJob {
                         batch_id,
                         merkle_root,
                         service_id,
                         retry_count: 0,
                     }).await;

                     batch_cache.clear();
                }
            }
        });
    });

    // 4. Adaptive WAL Checkpoint Task
    let pool_for_checkpoint = pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Ok(conn) = pool_for_checkpoint.get() {
                // Adaptive Checkpoint: passive first
                let _ = conn.execute("PRAGMA wal_checkpoint(PASSIVE);", []);
                println!("[DB] Periodic Maintenance: WAL Checkpoint completed.");
            }
        }
    });

    let state = Arc::new(AppState { pool, config, rpc_tx, log_tx });

    let app = Router::new()
        .route("/api/health", get(health_check))
        .route("/api/stats", get(get_stats))
        .route("/api/logs", get(get_logs))
        .route("/api/batches", get(get_batches))
        .route("/api/batches/:id/logs", get(get_batch_logs))
        .route("/api/batches/:id/verify", get(verify_batch))
        .route("/api/collect", post(collect_log))
        .route("/api/tamper", post(tamper_data))
        .nest_service("/", ServeDir::new(&dashboard_path))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Agent Server running on port 3000");
    axum::serve(listener, app).await.unwrap();
}

// Handlers
async fn health_check(State(state): State<Arc<AppState>>) -> Json<Value> {
    let pool_status = if state.pool.get().is_ok() { "UP" } else { "DOWN" };
    Json(json!({ "status": "ok", "data": { "database": pool_status, "env": state.config.env } }))
}

async fn get_stats(State(state): State<Arc<AppState>>) -> Json<Value> {
    let conn = state.pool.get().unwrap();
    let total_logs: i64 = conn.query_row("SELECT COUNT(*) FROM audit_logs", [], |r| r.get(0)).unwrap_or(0);
    let total_batches: i64 = conn.query_row("SELECT COUNT(*) FROM batches", [], |r| r.get(0)).unwrap_or(0);
    let tampered: i64 = conn.query_row("SELECT COUNT(*) FROM batches WHERE verify_status = 'TAMPERED'", [], |r| r.get(0)).unwrap_or(0);
    
    Json(json!({
        "status": "ok",
        "data": {
            "total_logs": total_logs,
            "total_batches": total_batches,
            "tampered_batches": tampered
        }
    }))
}

async fn get_logs(State(state): State<Arc<AppState>>, Query(q): Query<LogQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(50);
    let offset = q.offset.unwrap_or(0);
    let conn = state.pool.get().unwrap();
    
    let mut stmt = conn.prepare("SELECT service, user_id, amount, timestamp, hash, batch_id, event_type FROM audit_logs ORDER BY id DESC LIMIT ?1 OFFSET ?2").unwrap();
    let logs_iter = stmt.query_map([limit, offset], |row| {
        Ok(AuditLog {
            id: None,
            service: row.get(0)?,
            user_id: row.get(1)?,
            amount: row.get(2)?,
            timestamp: row.get(3)?,
            hash: row.get(4)?,
            batch_id: row.get(5)?,
            event_type: row.get(6)?,
            actor_id: None,
        })
    }).unwrap();

    let logs: Vec<_> = logs_iter.map(|l| l.unwrap()).collect();
    Json(json!({ "status": "ok", "data": logs, "meta": { "limit": limit, "offset": offset, "total": 0 } }))
}

async fn get_batches(State(state): State<Arc<AppState>>) -> Json<Value> {
    let conn = state.pool.get().unwrap();
    let mut stmt = conn.prepare("SELECT id, merkle_root, chain_status, verify_status, tx_hash, created_at FROM batches ORDER BY created_at DESC LIMIT 20").unwrap();
    let iter = stmt.query_map([], |row| {
        Ok(json!({
            "id": row.get::<_, String>(0)?,
            "merkle_root": row.get::<_, String>(1)?,
            "chain_status": row.get::<_, String>(2)?,
            "verify_status": row.get::<_, String>(3)?,
            "tx_hash": row.get::<_, Option<String>>(4)?,
            "created_at": row.get::<_, String>(5)?,
        }))
    }).unwrap();
    
    let batches: Vec<_> = iter.map(|b| b.unwrap()).collect();
    Json(json!({ "status": "ok", "data": batches }))
}

async fn get_batch_logs(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    let conn = state.pool.get().unwrap();
    let mut stmt = conn.prepare("SELECT service, user_id, amount, timestamp, hash FROM audit_logs WHERE batch_id = ?1").unwrap();
    let iter = stmt.query_map([id], |row| {
        Ok(AuditLog {
            id: None,
            service: row.get(0)?,
            user_id: row.get(1)?,
            amount: row.get(2)?,
            timestamp: row.get(3)?,
            hash: Some(row.get(4)?),
            batch_id: None,
            event_type: None,
            actor_id: None,
        })
    }).unwrap();
    
    let logs: Vec<_> = iter.map(|l| l.unwrap()).collect();
    Json(json!({ "status": "ok", "data": logs }))
}

async fn verify_batch(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Json<Value> {
    let conn = state.pool.get().unwrap();
    let batch_info: rusqlite::Result<(String, String)> = conn.query_row(
        "SELECT merkle_root, chain_status FROM batches WHERE id = ?1",
        [&id],
        |r| Ok((r.get(0)?, r.get(1)?))
    );

    if let Ok((stored_root, chain_status)) = batch_info {
        if chain_status != "COMMITTED" && state.config.env != "development" {
             return Json(json!({ "status": "error", "error": { "code": "NOT_COMMITTED", "message": "Batch not yet anchored to blockchain" } }));
        }

        let mut stmt = conn.prepare("SELECT service, user_id, amount, timestamp, hash FROM audit_logs WHERE batch_id = ?1").unwrap();
        let logs_iter = stmt.query_map([&id], |row| {
             Ok(AuditLog {
                 id: None, service: row.get(0)?, user_id: row.get(1)?, amount: row.get(2)?,
                 timestamp: row.get(3)?, hash: Some(row.get(4)?), batch_id: None, event_type: None, actor_id: None,
             })
        }).unwrap();
        
        let mut logs: Vec<_> = logs_iter.map(|l| {
            let mut log = l.unwrap();
            // Critical: Recompute hash from current DB fields to detect field-level tampering
            log.hash = Some(log.compute_hash());
            log
        }).collect();
        
        let computed_root = generate_merkle_root(&logs);
        
        let status = if computed_root == stored_root { "VALID" } else { "TAMPERED" };
        let _ = conn.execute("UPDATE batches SET verify_status = ?1 WHERE id = ?2", rusqlite::params![status, id]);
        
        Json(json!({ "status": "ok", "data": { "batch_id": id, "verified": status == "VALID", "computed_root": computed_root, "stored_root": stored_root } }))
    } else {
        Json(json!({ "status": "error", "error": { "code": "NOT_FOUND", "message": "Batch not found" } }))
    }
}

async fn collect_log(State(state): State<Arc<AppState>>, Json(mut payload): Json<AuditLog>) -> Json<Value> {
    payload.hash = Some(payload.compute_hash());
    if state.log_tx.try_send(payload).is_ok() {
        Json(json!({ "status": "ok", "data": { "received": true } }))
    } else {
        Json(json!({ "status": "error", "error": { "code": "QUEUE_FULL", "message": "Internal buffer full" } }))
    }
}

async fn tamper_data(State(state): State<Arc<AppState>>, Json(payload): Json<Value>) -> Json<Value> {
    if state.config.env != "development" {
        return Json(json!({ "status": "error", "error": { "code": "UNAUTHORIZED", "message": "Tampering only allowed in development mode" } }));
    }
    
    let batch_id = payload["batch_id"].as_str().unwrap_or("");
    let conn = state.pool.get().unwrap();
    
    // Manually tamper one log in this batch
    let res = conn.execute(
        "UPDATE audit_logs SET amount = 999999.0 WHERE batch_id = ?1 LIMIT 1",
        [batch_id]
    );

    match res {
        Ok(_) => {
            let _ = conn.execute(
                "INSERT INTO audit_logs (service, user_id, amount, timestamp, hash, event_type, actor_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params!["system", "tamper-tool", 0.0, chrono::Utc::now().to_rfc3339(), "", "TAMPER", format!("Manual manipulation on batch {}", batch_id)],
            );
            Json(json!({ "status": "ok", "data": { "tampered": true, "batch_id": batch_id } }))
        },
        Err(e) => {
            Json(json!({ "status": "error", "error": { "code": "DB_ERR", "message": e.to_string() } }))
        }
    }
}
