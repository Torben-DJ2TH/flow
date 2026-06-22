//! DAPNET inbound-message forwarding.
//!
//! The receiver uses the DAPNET RWTH core transmitter TCP protocol. It does not transmit POCSAG;
//! it only consumes incoming calls from the core feed, acknowledges them, normalizes the message,
//! and forwards it through existing FlowStation paths.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::thread;
use std::time::Duration;

use tetra_config::bluestation::{CfgDapnet, SharedConfig};

use crate::net_control::commands::ControlCommand;
use crate::net_telegram::TelegramAlertSink;
use crate::net_telemetry::{TelemetryEvent, TelemetrySink};
use crate::tpg2200::{build_sds_text_payload, build_tpg2200_callout_payload, format_hex_bytes};

type CmdSender = crossbeam_channel::Sender<ControlCommand>;

const TCP_READ_TIMEOUT: Duration = Duration::from_secs(30);
const CALLOUT_TEXT_MAX_CHARS: usize = 80;

#[derive(Debug, Clone)]
struct DapnetMessage {
    id: String,
    callsign: String,
    recipient: String,
    text: String,
    timestamp: String,
    priority: Option<u8>,
    msg_type: u8,
    speed: Option<u8>,
    ric: Option<u32>,
    function: Option<u8>,
}

pub fn spawn_dapnet_worker(
    cfg: SharedConfig,
    cmce_cmd_tx: Option<CmdSender>,
    telegram_sink: Option<TelegramAlertSink>,
    telemetry_sink: Option<TelemetrySink>,
) -> Option<thread::JoinHandle<()>> {
    match thread::Builder::new()
        .name("dapnet-worker".into())
        .spawn(move || DapnetWorker::new(cfg, cmce_cmd_tx, telegram_sink, telemetry_sink).run())
    {
        Ok(handle) => Some(handle),
        Err(err) => {
            tracing::warn!("DAPNET: failed to spawn worker thread: {}", err);
            None
        }
    }
}

struct DapnetWorker {
    cfg: SharedConfig,
    cmce_cmd_tx: Option<CmdSender>,
    telegram_sink: Option<TelegramAlertSink>,
    telemetry_sink: Option<TelemetrySink>,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    next_callout_incident: u16,
    last_callout_incident_base: Option<u16>,
    last_enabled: Option<bool>,
}

impl DapnetWorker {
    fn new(
        cfg: SharedConfig,
        cmce_cmd_tx: Option<CmdSender>,
        telegram_sink: Option<TelegramAlertSink>,
        telemetry_sink: Option<TelemetrySink>,
    ) -> Self {
        let next_callout_incident = cfg.effective_dapnet().callout_incident_base.clamp(1, 256);
        Self {
            cfg,
            cmce_cmd_tx,
            telegram_sink,
            telemetry_sink,
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            next_callout_incident,
            last_callout_incident_base: None,
            last_enabled: None,
        }
    }

    fn run(&mut self) {
        loop {
            let dapnet = self.cfg.effective_dapnet();
            let sleep = Duration::from_secs(dapnet.effective_poll_interval_secs());

            if !dapnet.enabled {
                if self.last_enabled != Some(false) {
                    tracing::info!("DAPNET integration disabled");
                    self.last_enabled = Some(false);
                }
                thread::sleep(sleep);
                continue;
            }
            if self.last_enabled != Some(true) {
                tracing::info!(
                    "DAPNET integration enabled (rwth_core={}, forward_sds={}, forward_callout={}, forward_telegram={})",
                    dapnet.rwth_core_enabled,
                    dapnet.forward_sds,
                    dapnet.forward_callout,
                    dapnet.forward_telegram
                );
                if !(dapnet.forward_sds || dapnet.forward_callout || dapnet.forward_telegram) {
                    tracing::warn!("DAPNET: enabled but no forwarding target is enabled");
                }
                self.last_callout_incident_base = None;
                self.last_enabled = Some(true);
            }
            let incident_base = dapnet.callout_incident_base.clamp(1, 256);
            if self.last_callout_incident_base != Some(incident_base) {
                self.next_callout_incident = incident_base;
                self.last_callout_incident_base = Some(incident_base);
            }

            if dapnet.rwth_core_enabled {
                if let Err(err) = self.run_rwth_core(&dapnet) {
                    tracing::warn!("DAPNET: RWTH core receive failed: {}", err);
                }
            } else {
                tracing::warn!(
                    "DAPNET: enabled, but rwth_core_enabled=false; no inbound receiver is active (api_url={})",
                    dapnet.api_url
                );
            }

            thread::sleep(sleep);
        }
    }

    fn run_rwth_core(&mut self, dapnet: &CfgDapnet) -> Result<(), String> {
        let host = dapnet.rwth_core_host.trim();
        let callsign = dapnet.rwth_core_callsign.trim();
        let authkey = dapnet.rwth_core_authkey.as_ref().trim();
        if host.is_empty() {
            return Err("RWTH core host is empty".to_string());
        }
        if callsign.is_empty() {
            return Err("RWTH core callsign is empty".to_string());
        }
        if authkey.is_empty() {
            return Err("RWTH core authkey is empty".to_string());
        }

        let addr = format!("{}:{}", host, dapnet.rwth_core_port);
        tracing::info!("DAPNET: connecting to RWTH core {} as {}", addr, callsign);
        let mut stream = TcpStream::connect(&addr).map_err(|e| format!("connect {} failed: {}", addr, e))?;
        if let Err(err) = stream.set_read_timeout(Some(TCP_READ_TIMEOUT)) {
            tracing::warn!("DAPNET: could not set TCP read timeout: {}", err);
        }

        self.write_login(&mut stream, dapnet)?;
        let reader_stream = stream
            .try_clone()
            .map_err(|e| format!("failed to clone RWTH core TCP stream: {}", e))?;
        let mut reader = BufReader::new(reader_stream);
        let mut logged_in = false;

        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => return Err("RWTH core closed connection".to_string()),
                Ok(_) => {
                    let line = line.trim_end_matches(|c| c == '\r' || c == '\n');
                    if line.is_empty() {
                        continue;
                    }
                    match self.handle_rwth_line(dapnet, &mut stream, line, &mut logged_in) {
                        Ok(()) => {}
                        Err(err) => return Err(err),
                    }
                }
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue;
                }
                Err(err) => return Err(format!("read failed: {}", err)),
            }
        }
    }

    fn write_login(&self, stream: &mut TcpStream, dapnet: &CfgDapnet) -> Result<(), String> {
        let device = non_empty_or(&dapnet.rwth_core_device, "FlowStation");
        let version = dapnet_version(&dapnet.rwth_core_version);
        let callsign = dapnet.rwth_core_callsign.trim().to_ascii_lowercase();
        let authkey = dapnet.rwth_core_authkey.as_ref().trim();
        let login = format!("[{} {} {} {}]\r\n", device, version, callsign, authkey);
        write_wire(stream, &login)
    }

    fn handle_rwth_line(
        &mut self,
        dapnet: &CfgDapnet,
        stream: &mut TcpStream,
        line: &str,
        logged_in: &mut bool,
    ) -> Result<(), String> {
        if line.starts_with('+') {
            return Ok(());
        }
        if line.starts_with('-') {
            tracing::warn!("DAPNET: RWTH core reported an error");
            return Ok(());
        }
        if line.starts_with('2') {
            if !*logged_in {
                tracing::info!("DAPNET: logged into RWTH core");
                *logged_in = true;
            }
            write_wire(stream, &format!("{line}:0000\r\n+\r\n"))?;
            return Ok(());
        }
        if line.starts_with('3') {
            write_wire(stream, "+\r\n")?;
            return Ok(());
        }
        if let Some(schedule) = line.strip_prefix("4:") {
            tracing::info!("DAPNET: RWTH core schedule received ({})", schedule);
            return Ok(());
        }
        if line.starts_with('7') {
            return Err(format!("login rejected by RWTH core: {}", sanitize_log_line(line)));
        }
        if line.starts_with('#') {
            self.handle_rwth_message(dapnet, stream, line)?;
            return Ok(());
        }

        tracing::warn!("DAPNET: unknown RWTH core message type '{}'", sanitize_log_line(line));
        write_wire(stream, "-\r\n")
    }

    fn handle_rwth_message(
        &mut self,
        dapnet: &CfgDapnet,
        stream: &mut TcpStream,
        line: &str,
    ) -> Result<(), String> {
        let msg_id = match rwth_line_id(line) {
            Some(id) => id,
            None => {
                tracing::warn!("DAPNET: malformed RWTH core message without valid id");
                write_wire(stream, "-\r\n")?;
                return Ok(());
            }
        };
        let ack_id = msg_id.wrapping_add(1);
        match parse_rwth_message(line) {
            Ok(message) => {
                write_wire(stream, &format!("#{ack_id:02X} +\r\n"))?;
                if message.msg_type != 6 {
                    tracing::debug!(
                        "DAPNET: ignoring non-text RWTH core message id={} type={}",
                        message.id,
                        message.msg_type
                    );
                    return Ok(());
                }
                if !self.remember_seen(&message.id, dapnet.effective_messages_limit()) {
                    tracing::debug!("DAPNET: duplicate message id={} ignored", message.id);
                    return Ok(());
                }
                self.forward_message(dapnet, &message);
                Ok(())
            }
            Err(err) => {
                tracing::warn!("DAPNET: malformed RWTH core message: {}", err);
                write_wire(stream, &format!("#{ack_id:02X} -\r\n"))
            }
        }
    }

    fn remember_seen(&mut self, id: &str, limit: usize) -> bool {
        if !self.seen.insert(id.to_string()) {
            return false;
        }
        self.seen_order.push_back(id.to_string());
        while self.seen_order.len() > limit {
            if let Some(old) = self.seen_order.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    fn forward_message(&mut self, dapnet: &CfgDapnet, msg: &DapnetMessage) {
        let mut paths: Vec<&str> = Vec::new();

        if dapnet.forward_sds {
            match self.forward_sds(dapnet, msg) {
                Ok(()) => paths.push("sds"),
                Err(err) => tracing::warn!("DAPNET: SDS forward failed for id={}: {}", msg.id, err),
            }
        }

        if dapnet.forward_callout {
            match self.forward_callout(dapnet, msg) {
                Ok(()) => paths.push("callout"),
                Err(err) => tracing::warn!("DAPNET: Call-Out forward failed for id={}: {}", msg.id, err),
            }
        }

        if dapnet.forward_telegram {
            match self.forward_telegram(dapnet, msg) {
                Ok(()) => paths.push("telegram"),
                Err(err) => tracing::warn!("DAPNET: Telegram forward failed for id={}: {}", msg.id, err),
            }
        }

        if paths.is_empty() {
            tracing::info!("DAPNET: received id={} recipient={} with no successful forwarding target", msg.id, msg.recipient);
        } else {
            tracing::info!(
                "DAPNET: forwarded id={} recipient={} callsign={} paths={} timestamp={} priority={:?}",
                msg.id,
                msg.recipient,
                msg.callsign,
                paths.join(","),
                msg.timestamp,
                msg.priority
            );
        }
        if let Some(sink) = &self.telemetry_sink {
            sink.send(TelemetryEvent::DapnetLog {
                direction: "rx".to_string(),
                id: msg.id.clone(),
                callsign: msg.callsign.clone(),
                recipient: msg.recipient.clone(),
                text: msg.text.clone(),
                priority: msg.priority,
                paths: paths.into_iter().map(|p| p.to_string()).collect(),
            });
        }
    }

    fn forward_sds(&self, dapnet: &CfgDapnet, msg: &DapnetMessage) -> Result<(), String> {
        if dapnet.sds_dest_issi == 0 {
            return Err("sds_dest_issi is 0".to_string());
        }
        let Some(tx) = &self.cmce_cmd_tx else {
            return Err("CMCE control sender unavailable".to_string());
        };
        let text = format_plain_message(&msg.callsign, &msg.text);
        let (len_bits, payload) = build_sds_text_payload(&text);
        tx.send(ControlCommand::SendSds {
            handle: 0,
            source_ssi: dapnet.sds_source_issi,
            dest_ssi: dapnet.sds_dest_issi,
            dest_is_group: dapnet.sds_dest_is_group,
            len_bits,
            payload,
        })
        .map_err(|e| format!("send to CMCE failed: {}", e))
    }

    fn forward_callout(&mut self, dapnet: &CfgDapnet, msg: &DapnetMessage) -> Result<(), String> {
        if dapnet.callout_dest_issi == 0 {
            return Err("callout_dest_issi is 0".to_string());
        }
        let Some(tx) = self.cmce_cmd_tx.clone() else {
            return Err("CMCE control sender unavailable".to_string());
        };
        let incident = self.next_incident();
        let callout_text = prefixed_text(&dapnet.callout_text_prefix, &msg.text);
        let (callout_text, truncated) = truncate_chars(&callout_text, CALLOUT_TEXT_MAX_CHARS);
        if truncated {
            tracing::warn!(
                "DAPNET: TPG2200 Call-Out text for id={} truncated to {} chars",
                msg.id,
                CALLOUT_TEXT_MAX_CHARS
            );
        }
        let payload = build_tpg2200_callout_payload(incident, &callout_text);
        if payload.len() > (u16::MAX as usize / 8) {
            return Err(format!("payload too large ({} bytes)", payload.len()));
        }
        tracing::debug!(
            "DAPNET: TPG2200 Call-Out id={} incident={} dest={} payload=[{}]",
            msg.id,
            incident,
            dapnet.callout_dest_issi,
            format_hex_bytes(&payload)
        );
        tx.send(ControlCommand::SendRawSdsType4 {
            handle: 0,
            source_ssi: dapnet.callout_source_issi,
            dest_ssi: dapnet.callout_dest_issi,
            dest_is_group: false,
            len_bits: (payload.len() * 8) as u16,
            payload,
        })
        .map_err(|e| format!("send to CMCE failed: {}", e))
    }

    fn forward_telegram(&self, dapnet: &CfgDapnet, msg: &DapnetMessage) -> Result<(), String> {
        let Some(sink) = &self.telegram_sink else {
            return Err("Telegram alerter unavailable".to_string());
        };
        sink.send_dapnet(dapnet.telegram_prefix.clone(), msg.callsign.clone(), msg.text.clone());
        Ok(())
    }

    fn next_incident(&mut self) -> u16 {
        let incident = self.next_callout_incident.clamp(1, 256);
        self.next_callout_incident = if incident >= 256 { 1 } else { incident + 1 };
        incident
    }
}

fn write_wire(stream: &mut TcpStream, text: &str) -> Result<(), String> {
    stream
        .write_all(text.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| format!("write failed: {}", e))
}

fn dapnet_version(version: &str) -> String {
    let trimmed = version.trim();
    if trimmed.is_empty() {
        "v1.0".to_string()
    } else if trimmed.starts_with('v') || trimmed.starts_with('V') {
        trimmed.to_string()
    } else {
        format!("v{trimmed}")
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() { fallback.to_string() } else { trimmed.to_string() }
}

fn rwth_line_id(line: &str) -> Option<u8> {
    let id = line.get(1..3)?;
    u8::from_str_radix(id, 16).ok()
}

fn parse_rwth_message(line: &str) -> Result<DapnetMessage, String> {
    let msg_id = rwth_line_id(line).ok_or_else(|| "invalid message id".to_string())?;
    let body = line
        .get(4..)
        .ok_or_else(|| "message line too short".to_string())?;
    let parts: Vec<&str> = body.splitn(5, ':').collect();
    if parts.len() != 5 {
        return Err("expected five colon-separated fields".to_string());
    }
    let msg_type = parts[0]
        .parse::<u8>()
        .map_err(|_| format!("invalid message type '{}'", parts[0]))?;
    let speed = parts[1].parse::<u8>().ok();
    let ric = u32::from_str_radix(parts[2], 16).ok();
    let function = parts[3].parse::<u8>().ok();
    let text = normalize_text(parts[4]);
    if text.is_empty() {
        return Err("empty message text".to_string());
    }
    let recipient = match (ric, function) {
        (Some(ric), Some(function)) => format!("RIC {ric} / func {function}"),
        (Some(ric), None) => format!("RIC {ric}"),
        _ => parts[2].to_string(),
    };
    let callsign = extract_callsign(&text).unwrap_or_else(|| recipient.clone());
    let id = format!("rwth:{msg_id:02X}:{}", stable_hash_hex(body));
    Ok(DapnetMessage {
        id,
        callsign,
        recipient,
        text,
        timestamp: chrono::Utc::now().to_rfc3339(),
        priority: None,
        msg_type,
        speed,
        ric,
        function,
    })
}

fn stable_hash_hex(input: &str) -> String {
    let digest = md5::compute(input.as_bytes());
    format!("{digest:x}")
}

fn normalize_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\t'))
        .collect::<String>()
        .trim()
        .to_string()
}

fn extract_callsign(text: &str) -> Option<String> {
    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '/');
        if cleaned.len() < 3 || cleaned.len() > 12 {
            continue;
        }
        let has_alpha = cleaned.chars().any(|c| c.is_ascii_alphabetic());
        let has_digit = cleaned.chars().any(|c| c.is_ascii_digit());
        let valid = cleaned
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '/');
        if has_alpha && has_digit && valid {
            return Some(cleaned.to_ascii_uppercase());
        }
    }
    None
}

fn format_plain_message(callsign: &str, text: &str) -> String {
    let callsign = callsign.trim();
    let text = text.trim();
    if callsign.is_empty() {
        text.to_string()
    } else {
        format!("{callsign} - {text}")
    }
}

fn prefixed_text(prefix: &str, text: &str) -> String {
    let prefix = prefix.trim();
    let text = text.trim();
    if prefix.is_empty() {
        text.to_string()
    } else if text.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {text}")
    }
}

fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((idx, _)) => (text[..idx].to_string(), true),
        None => (text.to_string(), false),
    }
}

fn sanitize_log_line(line: &str) -> String {
    truncate_chars(line, 160).0
}

#[cfg(test)]
mod tests {
    use super::{
        dapnet_version, extract_callsign, format_plain_message, parse_rwth_message, prefixed_text,
        truncate_chars,
    };

    #[test]
    fn parse_rwth_text_message_normalizes_fields() {
        let msg = parse_rwth_message("#00 6:1:3EC:3:5357.0 EA5FIV von DL4MFF um 1933z").unwrap();
        assert_eq!(msg.msg_type, 6);
        assert_eq!(msg.speed, Some(1));
        assert_eq!(msg.ric, Some(0x3EC));
        assert_eq!(msg.function, Some(3));
        assert_eq!(msg.callsign, "EA5FIV");
        assert_eq!(msg.recipient, "RIC 1004 / func 3");
        assert!(msg.id.starts_with("rwth:00:"));
    }

    #[test]
    fn parse_rwth_message_keeps_colons_in_text() {
        let msg = parse_rwth_message("#01 6:1:3EC:3:Alarm: Pumpe: Test").unwrap();
        assert_eq!(msg.text, "Alarm: Pumpe: Test");
    }

    #[test]
    fn helpers_are_stable() {
        assert_eq!(dapnet_version("1.0"), "v1.0");
        assert_eq!(dapnet_version("v2"), "v2");
        assert_eq!(extract_callsign("foo dl1abc-9 bar"), Some("DL1ABC-9".to_string()));
        assert_eq!(format_plain_message("DL1ABC", "Hallo"), "DL1ABC - Hallo");
        assert_eq!(prefixed_text("DAPNET", "Alarm"), "DAPNET Alarm");
        assert_eq!(truncate_chars("äöü", 2), ("äö".to_string(), true));
    }
}
