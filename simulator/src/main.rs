use reqwest::Client;
use serde_json::json;
use std::time::Duration;
use tokio::time::sleep;
use rand::Rng;

#[tokio::main]
async fn main() {
    println!("Memulai simulasi log generation...");
    println!("Pastikan Agent Server (cargo run -p agent) sedang menyala di background.");
    
    let client = Client::new();
    let start = std::time::Instant::now();
    let mut rng = rand::thread_rng();

    while start.elapsed() < Duration::from_secs(5) {
        let amount: f64 = rng.gen_range(10.0..1000.0);
        let user_id = format!("U{}", rng.gen_range(100..999));
        let payload = json!({
            "service": "transaction-service",
            "user_id": user_id,
            "amount": amount,
            "timestamp": chrono::Utc::now().to_rfc3339()
        });

        let res = client.post("http://127.0.0.1:3000/api/collect")
            .json(&payload)
            .send()
            .await;
            
        if let Err(e) = res {
             println!("Error connection: {}", e);
             break;
        }
        
        sleep(Duration::from_millis(100)).await;
    }
    
    println!("Selesai menghasilkan dummy logs.");
}
