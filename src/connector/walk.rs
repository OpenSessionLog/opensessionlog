use std::path::Path;

use crate::error::Result;
use crate::model::SessionRef;

/// Walk `current` (and its subdirectories) looking for `.jsonl` files.
/// Each `.jsonl` file is probed with `probe`; those returning `Ok(Some(id))`
/// are kept as `SessionRef`s. `root` becomes the `project_path` of each ref.
///
/// Skips `subagents/` subdirectories (mirrors the existing behavior in all
/// three JSONL connectors).
///
/// `std::fs::read_dir` failures are silently tolerated (existing behavior:
/// the loop is guarded by `if let Ok(entries) = ...`), but probe errors and
/// IO errors raised while reading a candidate file are propagated.
pub fn discover_jsonl<F>(
    root: &Path,
    current: &Path,
    source: &str,
    probe: &F,
) -> Result<Vec<SessionRef>>
where
    F: Fn(&Path) -> Result<Option<String>>,
{
    let mut refs: Vec<SessionRef> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(current) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().map(|n| n == "subagents").unwrap_or(false) {
                    continue;
                }
                refs.extend(discover_jsonl(root, &path, source, probe)?);
            } else if path.extension().map(|e| e == "jsonl").unwrap_or(false) {
                if let Some(native_id) = probe(&path)? {
                    refs.push(SessionRef {
                        source: source.to_string(),
                        native_id,
                        path,
                        project_path: Some(root.to_path_buf()),
                    });
                }
            }
        }
    }
    Ok(refs)
}
