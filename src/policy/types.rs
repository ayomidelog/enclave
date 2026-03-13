use serde::{Deserialize, Serialize};

pub const POLICY_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub version: u32,
    #[serde(default = "default_allow_true")]
    pub default_allow: bool,
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    #[serde(default)]
    pub uid: Option<u32>,
    #[serde(default)]
    pub allow: Vec<String>,
    #[serde(default)]
    pub deny: Vec<String>,
}

fn default_allow_true() -> bool {
    true
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            version: POLICY_VERSION,
            default_allow: true,
            rules: Vec::new(),
        }
    }
}
