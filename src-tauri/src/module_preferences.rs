use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf, sync::OnceLock};

const DEFAULTS: &str = include_str!("module-preferences.json");
const MAX_BYTES: usize = 32 * 1024;
static FILE: OnceLock<PathBuf> = OnceLock::new();

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Preferences {
    pub priorities: BTreeMap<String, Rule>,
    #[serde(default)]
    pub excluded_discovered_skills: BTreeMap<String, Vec<String>>,
}

impl Preferences {
    pub(crate) fn excludes_skill(&self, source: &str, slug: &str) -> bool {
        self.excluded_discovered_skills
            .get(source)
            .is_some_and(|skills| skills.iter().any(|name| name == slug))
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Rule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refinement_label: Option<String>,
}

pub(crate) fn initialize(directory: PathBuf) {
    let _ = FILE.set(directory.join("module-preferences.json"));
}

fn parse(text: &str) -> Result<Preferences, String> {
    if text.len() > MAX_BYTES {
        return Err("Module preferences exceed 32 KiB.".into());
    }
    let preferences: Preferences = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if preferences.priorities.keys().any(|id| {
        id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }) {
        return Err("Use catalog IDs as the keys in priorities.".into());
    }
    Ok(preferences)
}

fn read() -> Result<Preferences, String> {
    let Some(path) = FILE.get() else {
        return parse(DEFAULTS);
    };
    match fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => parse(DEFAULTS),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn current() -> Preferences {
    read().unwrap_or_else(|_| parse(DEFAULTS).expect("valid built-in module preferences"))
}

#[tauri::command]
pub fn read_module_preferences(defaults: bool) -> Result<String, String> {
    let preferences = if defaults { parse(DEFAULTS)? } else { read()? };
    serde_json::to_string_pretty(&preferences).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_module_preferences(text: String) -> Result<String, String> {
    let preferences = parse(&text)?;
    let text = serde_json::to_string_pretty(&preferences).map_err(|error| error.to_string())?;
    if text.len() > MAX_BYTES {
        return Err("Module preferences exceed 32 KiB.".into());
    }
    let path = FILE
        .get()
        .ok_or("Module preferences directory is unavailable.")?;
    fs::create_dir_all(path.parent().ok_or("Invalid preferences path.")?)
        .map_err(|error| error.to_string())?;
    let temporary = path.with_file_name(format!("module-preferences-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(error) = fs::write(&temporary, &text).and_then(|_| fs::rename(&temporary, path)) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    Ok(text)
}
