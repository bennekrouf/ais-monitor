//! Scans a local checkout for `$(VarName)` references in pipeline YAML —
//! the local half of the variable-group cleanup check. A variable group
//! entry is only "safe to delete" if nothing in the pipeline repo still
//! references it, so this has to actually grep the files rather than trust
//! the variable group's own metadata.

use std::collections::HashSet;
use std::path::Path;

const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", "bin", "obj", ".vs", ".vscode"];

/// Recursively scan `root` for `*.yml`/`*.yaml` files and collect every
/// `$(VarName)` reference found in their text. Best-effort: unreadable
/// files/dirs are silently skipped rather than failing the whole scan.
pub fn scan_variable_references(root: &Path) -> HashSet<String> {
    let mut found = HashSet::new();
    if root.as_os_str().is_empty() || !root.is_dir() {
        return found;
    }
    let re = regex::Regex::new(r"\$\(([A-Za-z0-9_.\-]+)\)").unwrap();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if !SKIP_DIRS.contains(&name) {
                    stack.push(path);
                }
                continue;
            }
            let is_yaml = path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("yml") || e.eq_ignore_ascii_case("yaml"))
                .unwrap_or(false);
            if !is_yaml { continue; }
            let Ok(content) = std::fs::read_to_string(&path) else { continue };
            for cap in re.captures_iter(&content) {
                found.insert(cap[1].to_string());
            }
        }
    }
    found
}
