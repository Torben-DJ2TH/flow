//! Dashboard-side persistence and helpers for EchoLink settings.

use tetra_config::bluestation::EcholinkRuntimeOverride;

pub fn mask_secret(secret: &str) -> String {
    crate::net_dashboard::dapnet::mask_secret(secret)
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn string_array_toml(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("\"{}\"", toml_escape(v)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn u32_array_toml(values: &[u32]) -> String {
    values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
}

fn routes_toml(routes: &std::collections::BTreeMap<String, String>) -> String {
    routes
        .iter()
        .map(|(dial, target)| format!("\"{}\" = \"{}\"", toml_escape(dial), toml_escape(target)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rewrite (or insert) the `[echolink]` section in the TOML file. A `.echolink.bak` backup is made.
pub fn write_echolink_to_toml(config_path: &str, ov: &EcholinkRuntimeOverride) -> std::io::Result<()> {
    let original = std::fs::read_to_string(config_path)?;
    let section = format!(
        "[echolink]\n\
         enabled = {}\n\
         callsign = \"{}\"\n\
         password = \"{}\"\n\
         location = \"{}\"\n\
         status_text = \"{}\"\n\
         directory_servers = [{}]\n\
         directory_port = {}\n\
         bind_addr = \"{}\"\n\
         audio_port = {}\n\
         control_port = {}\n\n\
         inbound_enabled = {}\n\
         outbound_enabled = {}\n\
         outbound_prefix = \"{}\"\n\
         strip_outbound_prefix = {}\n\
         service_numbers = [{}]\n\n\
         default_tetra_source_issi = {}\n\
         default_tetra_dest_issi = {}\n\
         default_tetra_dest_is_group = {}\n\
         routes = {{{}}}\n\
         allowed_callsigns = [{}]\n\
         allowed_node_ids = [{}]\n\
         auto_connect = \"{}\"\n\
         reconnect_interval_secs = {}\n\
         max_session_secs = {}",
        ov.enabled,
        toml_escape(&ov.callsign),
        toml_escape(&ov.password),
        toml_escape(&ov.location),
        toml_escape(&ov.status_text),
        string_array_toml(&ov.directory_servers),
        ov.directory_port,
        toml_escape(&ov.bind_addr),
        ov.audio_port,
        ov.control_port,
        ov.inbound_enabled,
        ov.outbound_enabled,
        toml_escape(&ov.outbound_prefix),
        ov.strip_outbound_prefix,
        string_array_toml(&ov.service_numbers),
        ov.default_tetra_source_issi,
        ov.default_tetra_dest_issi,
        ov.default_tetra_dest_is_group,
        routes_toml(&ov.routes),
        string_array_toml(&ov.allowed_callsigns),
        u32_array_toml(&ov.allowed_node_ids),
        toml_escape(&ov.auto_connect),
        ov.reconnect_interval_secs.max(1),
        ov.max_session_secs.max(1),
    );

    let lines: Vec<&str> = original.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 24);
    let mut i = 0;
    let mut replaced = false;

    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("[echolink]") {
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

    let backup = format!("{config_path}.echolink.bak");
    let _ = std::fs::copy(config_path, &backup);
    std::fs::write(config_path, new_content)
}
