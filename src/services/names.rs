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
