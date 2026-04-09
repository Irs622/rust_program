use rusqlite::Connection;
use dotenv::dotenv;

fn main() {
    dotenv().ok();
    println!("Mencari record transaksi terakhir...");
    
    let db_path = std::env::var("DATABASE_URL").unwrap_or_else(|_| "verilog.db".to_string());
    let conn = Connection::open(&db_path).expect("Failed to open db");
    
    let result = conn.query_row(
        "SELECT id, amount FROM audit_logs ORDER BY id DESC LIMIT 1",
        [],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    );

    match result {
        Ok((id, amount)) => {
            println!("[SEBELUM] Log ID {} - Amount: {}", id, amount);
            let new_amount = 999999.0;
            
            conn.execute(
                "UPDATE audit_logs SET amount = ?1 WHERE id = ?2",
                rusqlite::params![new_amount, id]
            ).unwrap();
            
            let tampered_amount: f64 = conn.query_row(
                "SELECT amount FROM audit_logs WHERE id = ?1",
                [&id],
                |row| row.get(0)
            ).unwrap();
            
            println!("[SESUDAH] Log ID {} (TAMPERED) - Amount: {}", id, tampered_amount);
            println!("[INFO] Data di database telah dimanipulasi! Silakan jalankan module verifier.");
        },
        Err(_) => {
            println!("Belum ada data di database. Jalankan simulator terlebih dahulu.");
        }
    }
}
