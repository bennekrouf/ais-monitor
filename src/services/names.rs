use std::collections::HashMap;
use std::path::Path;

const FILENAME: &str = ".ais-monitor-names";

/// Save custom chain names to .ais-monitor-names
pub fn save(dir: &str, names: &HashMap<String, String>) {
    let path = Path::new(dir).join(FILENAME);
    let mut lines: Vec<String> = names
        .iter()
        .map(|(k, v)| format!("{} = {}", k, v))
        .collect();
    lines.sort();
    let content = lines.join("\n") + "\n";
    let _ = std::fs::write(path, content);
}

/// Load custom chain names. Returns empty map if file is absent or malformed.
/// Format: one `key = value` per line; lines without `=` are skipped.
///
/// Currently consumed only by the TUI frontend (`ais-monitor-tui`); the
/// desktop app never calls `load` directly. Allow `dead_code` so the
/// desktop build stays warning-clean.
#[allow(dead_code)]
pub fn load(dir: &str) -> HashMap<String, String> {
    let path = Path::new(dir).join(FILENAME);
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    content
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}
