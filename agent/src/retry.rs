use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FailedBatch {
    pub batch_id: String,
    pub merkle_root: String,
    pub service_id: String,
}

#[derive(Clone, Debug)]
pub struct RpcJob {
    pub batch_id: String,
    pub merkle_root: String,
    pub service_id: String,
    pub retry_count: u32,
}

#[derive(Clone, Debug)]
pub struct RpcResult {
    pub batch_id: String,
    pub tx_hash: Option<String>,
    pub status: String, // "COMMITTED" | "FAILED"
    pub error: Option<String>,
}
