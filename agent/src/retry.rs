use std::time::Duration;
use tokio::sync::mpsc;
use crate::sender::send_merkle_root_to_solana;

#[derive(Clone, Debug)]
pub struct FailedBatch {
    pub batch_id: String,
    pub merkle_root: String,
    pub service_id: String,
}

pub fn spawn_retry_worker(mut rx: mpsc::Receiver<FailedBatch>) {
    tokio::spawn(async move {
        println!("[Retry Worker] Active. Listening for failed batches...");
        let mut retry_cache = Vec::new();

        loop {
            // Check for new failed batches
            while let Ok(batch) = rx.try_recv() {
                retry_cache.push(batch);
            }

            if !retry_cache.is_empty() {
                println!("[Retry Worker] Attempting to resend {} batches...", retry_cache.len());
                let mut still_failed = Vec::new();

                for batch in retry_cache.drain(..) {
                    let success = send_merkle_root_to_solana(
                        &batch.batch_id, 
                        &batch.merkle_root, 
                        &batch.service_id
                    ).await;

                    if !success {
                        still_failed.push(batch);
                    }
                }

                retry_cache = still_failed;
            }

            // Sleep 5 seconds before retrying
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}
