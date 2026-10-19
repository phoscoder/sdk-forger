//! Saved instructions: a name and the text to send to the model, kept in the user's config folder.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub instruction: String,
}

/// `$XDG_CONFIG_HOME/sdkforge/presets.json`, falling back to `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("sdkforge").join("presets.json"))
}

/// A missing or unreadable file is an empty list, never an error.
pub fn load(path: &Path) -> Vec<Preset> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save(path: &Path, presets: &[Preset]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(presets).map_err(std::io::Error::other)?;
    std::fs::write(path, text)
}

/// Add a preset, or replace the one with the same name (compared ignoring case).
pub fn upsert(presets: &mut Vec<Preset>, preset: Preset) {
    match presets
        .iter_mut()
        .find(|p| p.name.eq_ignore_ascii_case(&preset.name))
    {
        Some(existing) => *existing = preset,
        None => presets.push(preset),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, text: &str) -> Preset {
        Preset {
            name: name.into(),
            instruction: text.into(),
        }
    }

    #[test]
    fn roundtrips_and_tolerates_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("sdkforge-presets-{}", std::process::id()));
        let path = dir.join("nested").join("presets.json");
        assert!(load(&path).is_empty());
        let list = vec![preset("strict", "no any types")];
        save(&path, &list).unwrap();
        assert_eq!(load(&path), list);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn upsert_replaces_by_name() {
        let mut list = vec![preset("Strict", "a")];
        upsert(&mut list, preset("strict", "b"));
        upsert(&mut list, preset("loose", "c"));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].instruction, "b");
    }
}
