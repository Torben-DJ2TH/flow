use serde::Deserialize;
use std::collections::HashMap;
use toml::Value;

/// RF/debug test routing. Disabled by default; intended for lab checks such as
/// forcing local echo onto a secondary carrier.
#[derive(Debug, Clone, Default)]
pub struct CfgRfTest {
    pub local_echo_carrier: Option<u16>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct CfgRfTestDto {
    pub local_echo_carrier: Option<u16>,

    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

pub fn apply_rf_test_patch(dto: CfgRfTestDto) -> CfgRfTest {
    CfgRfTest {
        local_echo_carrier: dto.local_echo_carrier,
    }
}
