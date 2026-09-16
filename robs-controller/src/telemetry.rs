//! Serial data-string telemetry feed (ROV nav strings over a COM port).
//!
//! A dedicated reader thread owns the `serialport` handle, buffers bytes into
//! complete lines, parses `KEY\tVALUE` records, and publishes a
//! [`TelemetrySnapshot`] behind an `Arc<RwLock<..>>` that the UI tick reads.
//! While running, a lost/unpluggable port is retried every 2 s (auto-
//! reconnect); stopping flips an `AtomicBool` the loop polls in small slices
//! so `stop_telemetry`'s join never blocks long.
//!
//! Parsed format (primary, `Data String Test 1.txt`): one record per line,
//! tab-separated key-value pairs:
//! `E\t100000.01\tN\t500000.01\tKP\t100.01\tD\t1/1/2022\tT\t0:00:01\tB\t0.1\tA\t0.1\tCP\t-800.01\tFG\t-100.01\tH\t0.1`
//! Two legacy star-delimited variants from the same capture folder
//! (`*F0010\t*E 229201\t…` and `F0.004*E 229201*N …*Z12*K…`) are also
//! tolerated by the tokenizer.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use parking_lot::RwLock;
use robs_profiles::settings::SerialTelemetrySettings;

use crate::RobsController;

/// Latest parsed record plus connection health, published by the reader
/// thread and read by the UI tick.
#[derive(Default, Clone)]
pub struct TelemetrySnapshot {
    /// Fields of the last complete record. Duplicate keys within a record
    /// (the legacy star formats reuse letters) — last one wins.
    pub fields: HashMap<String, String>,
    /// Local receipt time of the last complete record (`LAST CHECK`).
    pub last_line_at: Option<DateTime<Local>>,
    /// Total complete records parsed since the reader started.
    pub lines_received: u64,
    /// True while the port (or replay file) is open.
    pub port_open: bool,
    /// Last open failure, shown as reconnect context (cleared on success).
    pub last_error: Option<String>,
}

/// List the system's serial port names for the settings dialog.
pub fn available_serial_ports() -> Vec<String> {
    match serialport::available_ports() {
        Ok(ports) => ports.into_iter().map(|p| p.port_name).collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    }
}

/// Parse one record line into ordered `(key, value)` pairs.
///
/// Tolerates all three captured shapes:
/// - tab-separated pairs:      `E\t100000.01\tCP\t-800.01`
/// - star tokens, `*K VALUE`:  `*F0010\t*E  229201\t*D01/01/2015`
/// - star tokens, `*KVALUE`:   `F0.004*E 229201*Z12*T01:00:04*L`
///
/// A token whose key carries no inline value consumes the following token as
/// its value only when that token looks like a value (leading digit/sign) —
/// bare flag keys (`L`) and the next record's key (`*F0.005`) stay distinct.
/// Returns `None` for blank lines and lines with no extractable key.
pub fn parse_line(line: &str) -> Option<Vec<(String, String)>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Tokenize on tabs and `*` markers; each part is either `K`, `KV`, or `K V`.
    let mut tokens: Vec<&str> = Vec::new();
    for part in trimmed.split(['\t', '*']) {
        let part = part.trim();
        if !part.is_empty() {
            tokens.push(part);
        }
    }

    let mut out: Vec<(String, String)> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        let key_len = token.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        if key_len == 0 {
            // Stray non-key token (garbage between records); skip it.
            i += 1;
            continue;
        }
        let key = token[..key_len].to_ascii_uppercase();
        let rest = token[key_len..].trim();
        if !rest.is_empty() {
            out.push((key, rest.to_string()));
            i += 1;
        } else if i + 1 < tokens.len() && looks_like_value(tokens[i + 1]) {
            out.push((key, tokens[i + 1].trim().to_string()));
            i += 2;
        } else {
            out.push((key, String::new()));
            i += 1;
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// True when a token should be consumed as the previous key's value
/// (numeric-like: `100000.01`, `-800.01`, `0:00:01`, `1/1/2022`).
fn looks_like_value(token: &str) -> bool {
    match token.chars().next() {
        Some(c) => c.is_ascii_digit() || c == '+' || c == '-' || c == '.',
        None => false,
    }
}

/// Merge a parsed line into the snapshot (single record: last field wins on
/// duplicate keys).
fn publish_line(line: &str, snapshot: &RwLock<TelemetrySnapshot>) {
    let Some(pairs) = parse_line(line) else {
        return;
    };
    if pairs.is_empty() {
        return;
    }
    let mut snap = snapshot.write();
    snap.fields.clear();
    for (key, value) in pairs {
        snap.fields.insert(key, value);
    }
    snap.last_line_at = Some(Local::now());
    snap.lines_received += 1;
}

/// Sleep in small slices so a stop request is honored promptly (the join in
/// `stop_telemetry` blocks on this).
fn interruptible_sleep(total: Duration, stop: &AtomicBool) {
    let step = Duration::from_millis(100);
    let mut waited = Duration::ZERO;
    while waited < total && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(step.min(total - waited));
        waited += step;
    }
}

/// Spawn the reader thread. With `ROBS_TELEMETRY_TEST_FILE` set it replays a
/// captured `.txt` at 1 Hz instead of touching the port — lets the feature be
/// exercised end-to-end while HyperTerminal holds COM3.
pub(crate) fn spawn_reader(
    settings: SerialTelemetrySettings,
    stop: Arc<AtomicBool>,
    snapshot: Arc<RwLock<TelemetrySnapshot>>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("telemetry-serial".into())
        .spawn(move || {
            match std::env::var("ROBS_TELEMETRY_TEST_FILE") {
                Ok(path) => replay_file(&path, &stop, &snapshot),
                Err(_) => read_serial(&settings, &stop, &snapshot),
            }
            snapshot.write().port_open = false;
        })
        .expect("spawn telemetry reader thread")
}

/// Reconnect-looping serial reader: open the port, drain it into lines, and
/// on any failure close, publish the error, wait 2 s, and retry — forever,
/// until stopped.
fn read_serial(settings: &SerialTelemetrySettings, stop: &AtomicBool, snapshot: &RwLock<TelemetrySnapshot>) {
    'outer: loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let open_result = serialport::new(&settings.port, settings.baud)
            .timeout(Duration::from_millis(250))
            .open();
        match open_result {
            Ok(mut port) => {
                {
                    let mut snap = snapshot.write();
                    snap.port_open = true;
                    snap.last_error = None;
                }
                let mut chunk = [0u8; 512];
                let mut acc: Vec<u8> = Vec::new();
                let mut last_rx = Instant::now();
                loop {
                    if stop.load(Ordering::SeqCst) {
                        break 'outer;
                    }
                    match port.read(&mut chunk) {
                        Ok(0) => break, // device dropped
                        Ok(n) => {
                            acc.extend_from_slice(&chunk[..n]);
                            last_rx = Instant::now();
                            while let Some(pos) = acc.iter().position(|&b| b == b'\n') {
                                let line: Vec<u8> = acc.drain(..=pos).collect();
                                let line = String::from_utf8_lossy(&line[..pos]).into_owned();
                                publish_line(&line, snapshot);
                            }
                        }
                        Err(e) if e.kind() == ErrorKind::TimedOut => {}
                        Err(_) => break, // unplugged / driver error -> reconnect
                    }
                    // Newline-less streams (legacy star dumps): flush whatever
                    // accumulated once the line has stopped growing.
                    if !acc.is_empty() && last_rx.elapsed() > Duration::from_millis(1000) {
                        let line = String::from_utf8_lossy(&acc).into_owned();
                        acc.clear();
                        publish_line(&line, snapshot);
                    }
                }
            }
            Err(e) => {
                let mut snap = snapshot.write();
                snap.port_open = false;
                snap.last_error = Some(format!("{}: {}", settings.port, e));
            }
        }
        // Reconnect backoff.
        interruptible_sleep(Duration::from_secs(2), stop);
    }
}

/// Replay a captured capture-file line stream at 1 Hz, looping until stopped.
fn replay_file(path: &str, stop: &AtomicBool, snapshot: &RwLock<TelemetrySnapshot>) {
    let Ok(content) = std::fs::read_to_string(path) else {
        let mut snap = snapshot.write();
        snap.last_error = Some(format!("replay file unreadable: {path}"));
        return;
    };
    {
        let mut snap = snapshot.write();
        snap.port_open = true;
        snap.last_error = None;
    }
    loop {
        for line in content.lines() {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            publish_line(line, snapshot);
            interruptible_sleep(Duration::from_secs(1), stop);
        }
    }
}

impl RobsController {
    /// (Re)start the telemetry reader with the current settings.
    pub fn start_telemetry(&mut self) {
        self.stop_telemetry_inner();
        let settings = self.telemetry.settings.clone();
        let snapshot = Arc::clone(&self.telemetry.snapshot);
        let stop = Arc::new(AtomicBool::new(false));
        self.telemetry.stop_flag = Some(Arc::clone(&stop));
        self.telemetry.reader = Some(spawn_reader(settings, stop, snapshot));
        self.telemetry.running = true;
    }

    /// Stop the telemetry reader (and clear the live indicator immediately).
    pub fn stop_telemetry(&mut self) {
        self.stop_telemetry_inner();
        self.telemetry.running = false;
        self.telemetry.snapshot.write().port_open = false;
    }

    fn stop_telemetry_inner(&mut self) {
        if let Some(flag) = self.telemetry.stop_flag.take() {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(handle) = self.telemetry.reader.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(line: &str) -> HashMap<String, String> {
        parse_line(line).unwrap().into_iter().collect()
    }

    #[test]
    fn parses_primary_tab_format() {
        let f = fields(
            "E\t100000.01\tN\t500000.01\tKP\t100.01\tD\t1/1/2022\tT\t0:00:01\tB\t0.1\tA\t0.1\tCP\t-800.01\tFG\t-100.01\tH\t0.1",
        );
        assert_eq!(f["E"], "100000.01");
        assert_eq!(f["N"], "500000.01");
        assert_eq!(f["KP"], "100.01");
        assert_eq!(f["D"], "1/1/2022");
        assert_eq!(f["T"], "0:00:01");
        assert_eq!(f["B"], "0.1");
        assert_eq!(f["A"], "0.1");
        assert_eq!(f["CP"], "-800.01");
        assert_eq!(f["FG"], "-100.01");
        assert_eq!(f["H"], "0.1");
    }

    #[test]
    fn tolerates_crlf_and_whitespace() {
        let f = fields("E\t100000.01\tN\t500000.01\r\n");
        assert_eq!(f["E"], "100000.01");
        assert_eq!(f["N"], "500000.01");
    }

    #[test]
    fn parses_star_tab_format() {
        let f = fields(
            "*F0010\t*E  229201\t*N 2822709\t*Z 100\t*D 75\t*T 01:00:04\t*H 100001\t*C 850.10\t*D01/01/2015\t*G 1050.9",
        );
        assert_eq!(f["F"], "0010");
        assert_eq!(f["E"], "229201");
        assert_eq!(f["N"], "2822709");
        assert_eq!(f["Z"], "100");
        assert_eq!(f["H"], "100001");
        assert_eq!(f["C"], "850.10");
        // The legacy format reuses `D` (depth then date); last one wins.
        assert_eq!(f["D"], "01/01/2015");
        assert_eq!(f["G"], "1050.9");
    }

    #[test]
    fn parses_star_inline_format() {
        let f = fields("F0.004*E  229201*N 2822709*Z12*K   123*T01:00:04*D01/01/1995*L*F0.005");
        assert_eq!(f["F"], "0.005");
        assert_eq!(f["E"], "229201");
        assert_eq!(f["N"], "2822709");
        assert_eq!(f["Z"], "12");
        assert_eq!(f["K"], "123");
        assert_eq!(f["T"], "01:00:04");
        assert_eq!(f["D"], "01/01/1995");
        // Bare flag key: no value follows that looks numeric.
        assert_eq!(f["L"], "");
    }

    #[test]
    fn blank_and_keyless_lines_yield_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   \r\n").is_none());
        assert!(parse_line("*****").is_none());
    }

    #[test]
    fn garbage_does_not_panic() {
        // Unknown keys parse harmlessly; the rest of the token is the value.
        let f = fields("hello world 42 \u{7f}\u{1}");
        assert_eq!(f["HELLO"], "world 42 \u{7f}\u{1}");
    }

    #[test]
    fn lowercased_keys_are_normalized() {
        let f = fields("e\t1.0\tcp\t-800");
        assert_eq!(f["E"], "1.0");
        assert_eq!(f["CP"], "-800");
    }
}
