//! Dashboard-side persistence and helpers for DAPNET settings.
//!
//! Mirrors the Telegram/WX dashboard helpers: rewrite only the `[dapnet]` section while
//! preserving the rest of the active config file, and mask secrets before returning them to UI.

use tetra_config::bluestation::DapnetRuntimeOverride;

/// Mask a secret for display. Returns an empty string for an empty value.
pub fn mask_secret(secret: &str) -> String {
    let secret = secret.trim();
    if secret.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 10 {
        return "•".repeat(chars.len());
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Rewrite (or insert) the `[dapnet]` section in the TOML file. A `.dapnet.bak` backup is made.
pub fn write_dapnet_to_toml(
    config_path: &str,
    ov: &DapnetRuntimeOverride,
) -> std::io::Result<()> {
    let original = std::fs::read_to_string(config_path)?;
    let section = format!(
        "[dapnet]\n\
         enabled = {}\n\
         api_url = \"{}\"\n\
         username = \"{}\"\n\
         password = \"{}\"\n\
         poll_interval_secs = {}\n\n\
         forward_sds = {}\n\
         forward_callout = {}\n\
         forward_telegram = {}\n\n\
         sds_source_issi = {}\n\
         sds_dest_issi = {}\n\
         sds_dest_is_group = {}\n\n\
         callout_source_issi = {}\n\
         callout_dest_issi = {}\n\
         callout_incident_base = {}\n\
         callout_text_prefix = \"{}\"\n\n\
         telegram_prefix = \"{}\"\n\n\
         rwth_core_enabled = {}\n\
         rwth_core_host = \"{}\"\n\
         rwth_core_port = {}\n\
         rwth_core_device = \"{}\"\n\
         rwth_core_version = \"{}\"\n\
         rwth_core_callsign = \"{}\"\n\
         rwth_core_authkey = \"{}\"\n\
         rwth_messages_limit = {}",
        ov.enabled,
        toml_escape(&ov.api_url),
        toml_escape(&ov.username),
        toml_escape(&ov.password),
        ov.poll_interval_secs.max(1),
        ov.forward_sds,
        ov.forward_callout,
        ov.forward_telegram,
        ov.sds_source_issi,
        ov.sds_dest_issi,
        ov.sds_dest_is_group,
        ov.callout_source_issi,
        ov.callout_dest_issi,
        ov.callout_incident_base.clamp(1, 256),
        toml_escape(&ov.callout_text_prefix),
        toml_escape(&ov.telegram_prefix),
        ov.rwth_core_enabled,
        toml_escape(&ov.rwth_core_host),
        ov.rwth_core_port,
        toml_escape(&ov.rwth_core_device),
        toml_escape(&ov.rwth_core_version),
        toml_escape(&ov.rwth_core_callsign),
        toml_escape(&ov.rwth_core_authkey),
        ov.rwth_messages_limit.max(1),
    );

    let lines: Vec<&str> = original.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 32);
    let mut i = 0;
    let mut replaced = false;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("[dapnet]") {
            out.push(section.clone());
            replaced = true;
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim_start();
                if t.starts_with('[') && t.contains(']') {
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(lines[i].to_string());
        i += 1;
    }

    if !replaced {
        if !out.is_empty() && !out.last().map(|l| l.is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        out.push(section);
    }

    let mut new_content = out.join("\n");
    if original.ends_with('\n') {
        new_content.push('\n');
    }

    let backup = format!("{config_path}.dapnet.bak");
    let _ = std::fs::copy(config_path, &backup);
    std::fs::write(config_path, new_content)
}
