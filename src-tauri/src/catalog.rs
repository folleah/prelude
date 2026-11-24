use crate::domain::RegistryEntry;
use serde::Serialize;
use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_BYTES: u64 = 1024 * 1024;

#[derive(Serialize)]
pub struct Catalog {
    pub(crate) entries: Vec<RegistryEntry>,
    pub(crate) errors: Vec<String>,
    #[serde(rename = "installedIds")]
    pub(crate) installed_ids: Vec<&'static str>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum CatalogDocument {
    Entry(RegistryEntry),
    Entries(Vec<RegistryEntry>),
}

fn parse(bytes: &[u8], format: &str) -> Result<Vec<RegistryEntry>, String> {
    let document: CatalogDocument = match format {
        "json" => serde_json::from_slice(bytes).map_err(|e| e.to_string())?,
        "yaml" | "yml" => serde_yaml::from_slice(bytes).map_err(|e| e.to_string())?,
        _ => return Err("Expected a YAML or JSON file".into()),
    };
    Ok(match document {
        CatalogDocument::Entry(entry) => vec![entry],
        CatalogDocument::Entries(entries) => entries,
    })
}

impl Catalog {
    fn extend(&mut self, entries: Vec<RegistryEntry>) -> Result<(), String> {
        let mut ids: HashSet<_> = self.entries.iter().map(|entry| entry.id.as_str()).collect();
        for entry in &entries {
            if entry.schema_version != 1 {
                return Err(format!(
                    "{}: unsupported schema version {}",
                    entry.id, entry.schema_version
                ));
            }
            if entry.id.is_empty()
                || !entry
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                return Err(format!("Invalid component ID: {}", entry.id));
            }
            if !ids.insert(&entry.id) {
                return Err(format!("Duplicate component ID: {}", entry.id));
            }
        }
        self.entries.extend(entries);
        Ok(())
    }
}

fn read_extension(path: &Path) -> Result<Vec<RegistryEntry>, String> {
    let format = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(format.as_str(), "json" | "yaml" | "yml") {
        return Err("Expected a YAML or JSON file".into());
    }
    if !std::fs::metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("Expected a regular file".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Expected a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Catalog file exceeds 1 MiB".into());
    }
    parse(&bytes, &format)
}

#[tauri::command]
pub fn load_catalog(extension_paths: Vec<PathBuf>) -> Result<Catalog, String> {
    let mut catalog = Catalog {
        entries: Vec::new(),
        errors: Vec::new(),
        installed_ids: std::env::var_os("PATH")
            .map(|path| detect_installed(&std::env::split_paths(&path).collect::<Vec<_>>()))
            .unwrap_or_default(),
    };
    catalog
        .extend(parse(include_bytes!("catalog.yaml"), "yaml")?)
        .map_err(|e| format!("Built-in catalog: {e}"))?;
    for path in extension_paths {
        if let Err(error) = read_extension(&path).and_then(|entries| catalog.extend(entries)) {
            catalog.errors.push(format!("{}: {error}", path.display()));
        }
    }
    let known: HashSet<_> = catalog
        .entries
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();
    let cached = crate::skill_catalog::cached_entries()
        .into_iter()
        .filter(|entry| !known.contains(entry.id.as_str()))
        .collect();
    catalog.extend(cached).ok();
    Ok(catalog)
}

fn detect_installed(directories: &[PathBuf]) -> Vec<&'static str> {
    const COMMANDS: &[(&str, &str)] = &[
        ("codex", "codex"),
        ("claude-code", "claude"),
        ("opencode", "opencode"),
        ("herdr", "herdr"),
        ("agent-deck", "agent-deck"),
        ("cmux", "cmux"),
        ("rtk", "rtk"),
        ("ast-grep", "ast-grep"),
        ("repomix", "repomix"),
        ("headroom", "headroom"),
        ("openspec", "openspec"),
        ("docker", "docker"),
        ("git", "git"),
        ("nodejs", "node"),
        ("npm", "npm"),
        ("pnpm", "pnpm"),
        ("yarn", "yarn"),
        ("bun", "bun"),
        ("deno", "deno"),
        ("nginx", "nginx"),
        ("postgresql", "postgres"),
    ];
    COMMANDS
        .iter()
        .filter_map(|&(id, command)| {
            directories
                .iter()
                .any(|dir| dir.is_absolute() && command_exists(dir, command))
                .then_some(id)
        })
        .collect()
}

#[cfg(windows)]
fn command_exists(directory: &Path, command: &str) -> bool {
    ["exe", "cmd", "bat", "com"]
        .iter()
        .any(|extension| directory.join(command).with_extension(extension).is_file())
}

#[cfg(unix)]
fn command_exists(directory: &Path, command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    directory
        .join(command)
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_known_commands_without_running_them() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&dir).unwrap();
        let codex = dir.join(if cfg!(windows) { "codex.cmd" } else { "codex" });
        std::fs::write(&codex, "This fixture must never be executed.").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::create_dir(dir.join(if cfg!(windows) { "herdr.exe" } else { "herdr" })).unwrap();
        assert_eq!(detect_installed(std::slice::from_ref(&dir)), ["codex"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn loads_builtin_and_accepts_only_valid_extensions() {
        let mut catalog = load_catalog(vec![]).unwrap();
        for id in [
            "codex",
            "claude-code",
            "opencode",
            "graphify",
            "ponytail",
            "omniroute",
            "openspec",
        ] {
            assert!(catalog.entries.iter().any(|entry| entry.id == id));
        }
        let mut data = serde_json::to_value(&catalog.entries[0]).unwrap();
        let initial_count = catalog.entries.len();
        data["id"] = "local-tool".into();
        let json = serde_json::to_vec(&data).unwrap();
        catalog.extend(parse(&json, "json").unwrap()).unwrap();
        assert!(catalog
            .extend(parse(&json, "json").unwrap())
            .unwrap_err()
            .contains("Duplicate"));
        data["id"] = "other-tool".into();
        data["schemaVersion"] = 2.into();
        let yaml = serde_yaml::to_string(&data).unwrap();
        assert!(catalog
            .extend(parse(yaml.as_bytes(), "yaml").unwrap())
            .unwrap_err()
            .contains("unsupported schema"));
        assert_eq!(catalog.entries.len(), initial_count + 1);
    }
}

#[tauri::command]
pub fn open_module_source(url: String) -> Result<(), String> {
    let url = reqwest::Url::parse(&url).map_err(|_| "Invalid module URL.")?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return Err("The module URL must use HTTPS and contain no credentials.".into());
    }
    webbrowser::open(url.as_str()).map_err(|_| "Could not open the module URL.".into())
}
