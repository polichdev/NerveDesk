use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardPayload {
    Text(String),
    Image(Vec<u8>),
    Files(Vec<String>),
}

impl ClipboardPayload {
    pub fn compute_hash(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        match self {
            Self::Text(text) => {
                0u8.hash(&mut hasher);
                text.hash(&mut hasher);
            }
            Self::Image(bytes) => {
                1u8.hash(&mut hasher);
                bytes.hash(&mut hasher);
            }
            Self::Files(files) => {
                2u8.hash(&mut hasher);
                files.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}
