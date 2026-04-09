pub mod db;

use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};
use rs_merkle::{MerkleTree, Hasher};

#[derive(Clone)]
pub struct Keccak256Algorithm {}

impl Hasher for Keccak256Algorithm {
    type Hash = [u8; 32];

    fn hash(data: &[u8]) -> [u8; 32] {
        let mut hasher = Keccak256::new();
        hasher.update(data);
        let mut res = [0u8; 32];
        res.copy_from_slice(hasher.finalize().as_slice());
        res
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AuditLog {
    pub id: Option<i64>,
    pub service: String,
    pub user_id: String,
    pub amount: f64,
    pub timestamp: String,
    pub hash: Option<String>,
}

impl AuditLog {
    pub fn compute_hash(&self) -> String {
        let payload = format!("{}|{}|{}|{}", self.service, self.user_id, self.amount, self.timestamp);
        let hash_bytes = Keccak256Algorithm::hash(payload.as_bytes());
        hex::encode(hash_bytes)
    }
}

pub fn generate_merkle_root(logs: &[AuditLog]) -> String {
    if logs.is_empty() {
        let empty_hash = Keccak256Algorithm::hash(b"");
        return hex::encode(empty_hash);
    }

    let mut leaves: Vec<[u8; 32]> = Vec::new();
    for log in logs {
        if let Some(h) = &log.hash {
            if let Ok(bytes) = hex::decode(h) {
                let mut leaf = [0u8; 32];
                leaf.copy_from_slice(&bytes[..32]);
                leaves.push(leaf);
            }
        }
    }
    
    let merkle_tree = MerkleTree::<Keccak256Algorithm>::from_leaves(&leaves);
    
    if let Some(root) = merkle_tree.root() {
        hex::encode(root)
    } else {
        hex::encode(Keccak256Algorithm::hash(b""))
    }
}
