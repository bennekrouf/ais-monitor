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
