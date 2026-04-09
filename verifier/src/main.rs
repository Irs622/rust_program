use shared::{db::init_db, generate_merkle_root, AuditLog};
use dotenv::dotenv;

fn main() {
    dotenv().ok();
    println!("Memeriksa integritas seluruh batch histori via rs-merkle...");
    
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "verilog.db".to_string());
    let conn = init_db(&db_path).unwrap();

    let mut stmt = conn.prepare("SELECT id, merkle_root FROM batches ORDER BY created_at DESC").unwrap();
    let batch_iter = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }).unwrap();

    let mut has_tampering = false;

    for batch in batch_iter {
        let (batch_id, stored_root) = batch.unwrap();
        
        let mut log_stmt = conn.prepare("SELECT service, user_id, amount, timestamp, hash FROM audit_logs WHERE batch_id = ?1").unwrap();
        let logs_iter = log_stmt.query_map([&batch_id], |row| {
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
        for log in logs_iter {
            logs.push(log.unwrap());
        }

        let computed_root = generate_merkle_root(&logs);
        
        if computed_root == stored_root {
            println!("✅ [VALID] Batch {} - On-chain & Off-chain records match.", batch_id);
        } else {
            println!("❌ [TAMPERED] Batch {} - Data manipulation detected!", batch_id);
            println!("   Detail: On-chain: {}, Computed: {}", stored_root, computed_root);
            has_tampering = true;
        }
    }

    if has_tampering {
        println!("\n⚠️ PERINGATAN: TERDAPAT MANIPULASI DATA PADA HISTORI LOG!");
    } else {
        println!("\n🔒 AMAN: Seluruh log audit sesuai dengan Merkle Root di blockchain.");
    }
}
