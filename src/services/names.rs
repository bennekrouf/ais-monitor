use std::collections::HashMap;
use std::path::Path;

const FILENAME: &str = ".ais-monitor-names";

/// Load custom chain names from .ais-monitor-names in the project dir
pub fn load(dir: &str) -> HashMap<String, String> {
    let path = Path::new(dir).join(FILENAME);
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(&path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some((key, val)) = trimmed.split_once('=') {
                let k = key.trim().to_string();
                let v = val.trim().to_string();
                if !k.is_empty() && !v.is_empty() {
                    map.insert(k, v);
                }
            }
        }
    }
    map
}

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
