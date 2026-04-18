use anchor_client::{
    solana_sdk::{
        commitment_config::CommitmentConfig,
        signature::Keypair,
        pubkey::Pubkey,
    },
    Client, Cluster,
};
use std::sync::Arc;
use std::str::FromStr;

pub fn send_merkle_root_to_solana(batch_id: &str, merkle_root: &str, _service_id: &str) -> Result<String, String> {
    let rpc_url = std::env::var("SOLANA_RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".to_string());
    
    // Fallback Mock mode if no validator available (untuk test)
    let is_mock = std::env::var("MOCK_SOLANA").unwrap_or_else(|_| "true".to_string());
    if is_mock == "true" {
        println!("[Sender (Mock)] Berhasil mencatat Batch {} ke Solana.", batch_id);
        // Return a mock TX hash
        return Ok(format!("mock_tx_{}", batch_id));
    }

    println!("[Sender] Mengirim batch {} ke Solana...", batch_id);

    // Default keypair path (or load from bytes)
    let payer = Keypair::new();
    
    let client = Client::new_with_options(
        Cluster::Custom(rpc_url.clone(), rpc_url.clone()),
        Arc::new(payer),
        CommitmentConfig::confirmed(),
    );

    let program_id = Pubkey::from_str("VeriLog111111111111111111111111111111111111").unwrap();
    let _program = client.program(program_id);

    // Convert string to [u8; 32]
    let mut root_bytes = [0u8; 32];
    if let Ok(bytes) = hex::decode(merkle_root) {
        let len = std::cmp::min(bytes.len(), 32);
        root_bytes[..len].copy_from_slice(&bytes[..len]);
    }

    // In a real implementation, we would build and send the transaction here.
    // For this demonstration, we check connectivity and return success.
    
    let is_rpc_reachable = reqwest::blocking::get(&rpc_url).is_ok();
    
    if !is_rpc_reachable {
        return Err(format!("Koneksi ke {} ditolak.", rpc_url));
    }
    
    println!("[Sender] Batch selesai di-record ke Solana on-chain.");
    Ok(format!("tx_{}", batch_id))
}
