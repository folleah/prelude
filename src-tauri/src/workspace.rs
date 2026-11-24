use crate::domain::Plan;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use tauri::Manager;
use uuid::Uuid;

pub(crate) const PROJECT_FILE: &str = "prelude.yaml";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectYaml {
    #[serde(
        default,
        deserialize_with = "read_hooks",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub(crate) hook_checks: Vec<crate::ai_plan::PlannedHook>,
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    pub(crate) description: String,
    pub(crate) plan: Plan,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    messages: Option<Vec<ChatMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    discovery: Option<Discovery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    generation_input: Option<GenerationInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery_mode: Option<DeliveryMode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) local_skills: Vec<String>,
    #[serde(default)]
    assumptions: Vec<String>,
    #[serde(default, rename = "settings", skip_serializing)]
    _legacy_settings: Option<serde::de::IgnoredAny>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum DeliveryMode {
    Create,
    Apply,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ChatRole {
    User,
    Assistant,
    Error,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatMessage {
    role: ChatRole,
    text: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Discovery {
    description: String,
    questions: Vec<String>,
    answers: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct GenerationInput {
    prompt: String,
    answers: Vec<[String; 2]>,
    use_defaults: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_plan: Option<Plan>,
}

fn read_hooks<'de, D: serde::Deserializer<'de>>(
    source: D,
) -> Result<Vec<crate::ai_plan::PlannedHook>, D::Error> {
    use crate::ai_plan::{HookCheck, PlannedHook};

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SavedHook {
        Described(PlannedHook),
        Legacy(HookCheck),
    }

    Ok(Vec::<SavedHook>::deserialize(source)?
        .into_iter()
        .map(|hook| match hook {
            SavedHook::Described(hook) => hook,
            SavedHook::Legacy(check) => PlannedHook {
                title: String::new(),
                description: String::new(),
                check: Some(check),
            },
        })
        .collect())
}

fn check(project: ProjectYaml) -> Result<ProjectYaml, String> {
    if project.schema_version != 1 {
        return Err(format!(
            "YAML version {} is unsupported.",
            project.schema_version
        ));
    }
    if project.discovery.as_ref().is_some_and(|interview| {
        interview.questions.is_empty() || interview.answers.len() >= interview.questions.len()
    }) {
        return Err("The saved project contains an invalid discovery interview.".into());
    }
    Ok(project)
}

#[tauri::command]
pub fn encode_project_yaml(project: ProjectYaml) -> Result<String, String> {
    let yaml = serde_yaml::to_string(&check(project)?).map_err(|e| e.to_string())?;
    if yaml.len() > 1024 * 1024 {
        return Err("The project YAML exceeds 1 MiB.".into());
    }
    Ok(yaml)
}

#[tauri::command]
pub fn decode_project_yaml(yaml: String) -> Result<ProjectYaml, String> {
    if yaml.len() > 1024 * 1024 {
        return Err("The project YAML exceeds 1 MiB.".into());
    }
    check(serde_yaml::from_str(&yaml).map_err(|e| format!("Invalid project YAML: {e}"))?)
}

#[derive(Serialize, Deserialize)]
pub struct HistoryEntry {
    id: Uuid,
    name: String,
    path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct HistoryRecord {
    entry: HistoryEntry,
    project: ProjectYaml,
}

fn history_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("project-history"))
}

#[tauri::command]
pub fn save_project_history(
    app: tauri::AppHandle,
    id: String,
    path: PathBuf,
    project: ProjectYaml,
) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(|_| "Invalid project ID.")?;
    save_history(&history_dir(&app)?, id, path, project)
}

fn save_history(
    directory: &std::path::Path,
    id: Uuid,
    path: PathBuf,
    project: ProjectYaml,
) -> Result<(), String> {
    let path = fs::canonicalize(path).map_err(|e| e.to_string())?;
    let directory_name = path.file_name().ok_or("Select the project directory.")?;
    let name = project
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| directory_name.to_string_lossy().into_owned());
    let record = HistoryRecord {
        entry: HistoryEntry { id, name, path },
        project: check(project)?,
    };
    let yaml = serde_yaml::to_string(&record).map_err(|e| e.to_string())?;
    if yaml.len() > 1024 * 1024 {
        return Err("The project YAML exceeds 1 MiB.".into());
    }
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    write_history_file(directory.join(format!("{id}.yaml")), yaml.as_bytes())?;
    let entry = serde_json::to_vec(&record.entry).map_err(|e| e.to_string())?;
    write_history_file(directory.join(format!("{id}.json")), &entry)
}

fn write_history_file(path: PathBuf, contents: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = fs::write(&temporary, contents).and_then(|_| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_project_history(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(|_| "Invalid project ID.")?;
    remove_history(&history_dir(&app)?, id)
}

fn remove_history(directory: &std::path::Path, id: Uuid) -> Result<(), String> {
    for extension in ["json", "yaml"] {
        match fs::remove_file(directory.join(format!("{id}.{extension}"))) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not remove the recent project entry: {error}"
                ))
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn list_project_history(app: tauri::AppHandle) -> Result<Vec<HistoryEntry>, String> {
    list_history(&history_dir(&app)?)
}

fn list_history(path: &std::path::Path) -> Result<Vec<HistoryEntry>, String> {
    let directory = match fs::read_dir(path) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.to_string()),
    };
    let mut entries = Vec::new();
    for file in directory
        .flatten()
        .filter(|item| item.path().extension().is_some_and(|ext| ext == "json"))
    {
        if let Some(entry) = fs::read(file.path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HistoryEntry>(&bytes).ok())
        {
            entries.push((file.metadata().and_then(|meta| meta.modified()).ok(), entry));
        }
    }
    entries.sort_by_key(|(time, _)| std::cmp::Reverse(*time));
    Ok(entries.into_iter().map(|(_, entry)| entry).collect())
}

fn read_record(path: PathBuf) -> Result<HistoryRecord, String> {
    use std::io::Read;
    let mut yaml = String::new();
    fs::File::open(path)
        .and_then(|file| file.take(1_048_577).read_to_string(&mut yaml))
        .map_err(|e| format!("The saved YAML is unavailable: {e}"))?;
    if yaml.len() > 1_048_576 {
        return Err("The project YAML exceeds 1 MiB.".into());
    }
    serde_yaml::from_str(&yaml).map_err(|e| format!("Invalid project history YAML: {e}"))
}

#[tauri::command]
pub fn open_project_history(app: tauri::AppHandle, id: String) -> Result<ProjectYaml, String> {
    let id = Uuid::parse_str(&id).map_err(|_| "Invalid project ID.")?;
    check(read_record(history_dir(&app)?.join(format!("{id}.yaml")))?.project)
}

#[tauri::command]
pub fn open_project_snapshot(
    app: tauri::AppHandle,
    repository: PathBuf,
) -> Result<Option<(HistoryEntry, ProjectYaml)>, String> {
    let path = crate::preview::existing_project_directory(&repository)?;
    snapshot_for_path(&history_dir(&app)?, &path)
}

fn snapshot_for_path(
    directory: &std::path::Path,
    path: &std::path::Path,
) -> Result<Option<(HistoryEntry, ProjectYaml)>, String> {
    for entry in list_history(directory)?
        .into_iter()
        .filter(|entry| entry.path == path)
    {
        let record = match read_record(directory.join(format!("{}.yaml", entry.id))) {
            Ok(record) => record,
            Err(_) => continue,
        };
        if record.entry.path == path && record.entry.id == entry.id {
            if let Ok(project) = check(record.project) {
                return Ok(Some((entry, project)));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schemaVersion: 1
description: Sample
plan:
  harness: { id: codex, version: "0.167.1", reason: Selected }
  components:
    - { id: missing-tool, version: "1", reason: Custom }
    - { id: gpt-4-1, version: gpt-4.1-2025-04-14, reason: Model }
  roles:
    - { name: Developer, responsibility: Implement, interaction: Report, reason: Main, modelId: gpt-4-1 }
  instructions: [Run tests]
settings:
  budget: { amount: 5, currency: USD, periodDays: 30 }
  priority: 40
  providers: [openai]
  inputTokens: 83000
  outputTokens: 22000
  customRates: {}
"#;

    #[test]
    fn round_trip_preserves_assignments_and_unknown_entries_and_drops_legacy_settings() {
        let project = decode_project_yaml(SAMPLE.into()).unwrap();
        let yaml = encode_project_yaml(project).unwrap();
        assert!(!yaml.lines().any(|line| line.starts_with("settings:")));
        let project = decode_project_yaml(yaml).unwrap();
        assert_eq!(project.plan.roles[0].model_id.as_deref(), Some("gpt-4-1"));
        assert_eq!(project.plan.components[0].id, "missing-tool");
        assert!(
            decode_project_yaml(SAMPLE.replace("schemaVersion: 1", "schemaVersion: 99")).is_err()
        );
        assert!(decode_project_yaml("plan: [".into()).is_err());
        assert!(decode_project_yaml(format!("{SAMPLE}\napiKey: secret")).is_err());
    }

    #[test]
    fn round_trip_preserves_chat_and_pending_discovery() {
        let yaml = format!("{SAMPLE}\nmessages:\n  - {{ role: user, text: A blog }}\n  - {{ role: assistant, text: Where will you host it? }}\ndiscovery:\n  description: A blog\n  questions: [Static or dynamic?, Where will you host it?]\n  answers: [Static]\n");
        let project = decode_project_yaml(yaml).unwrap();
        let reopened = decode_project_yaml(encode_project_yaml(project).unwrap()).unwrap();
        let messages = reopened.messages.unwrap();
        assert!(matches!(messages[1].role, ChatRole::Assistant));
        assert_eq!(messages[1].text, "Where will you host it?");
        assert_eq!(reopened.discovery.unwrap().answers, ["Static"]);
        assert!(decode_project_yaml(SAMPLE.into())
            .unwrap()
            .messages
            .is_none());
    }

    #[test]
    fn round_trip_preserves_the_original_generation_prompt_and_answers() {
        let yaml = format!("{SAMPLE}\ngenerationInput:\n  prompt: Build a blog\n  answers: [[Static or dynamic?, Static]]\n  useDefaults: true\n");
        let project = decode_project_yaml(yaml).unwrap();
        let reopened = decode_project_yaml(encode_project_yaml(project).unwrap()).unwrap();
        let input = reopened.generation_input.unwrap();
        assert_eq!(input.prompt, "Build a blog");
        assert_eq!(input.answers, [["Static or dynamic?", "Static"]]);
        assert!(input.use_defaults);
    }

    #[test]
    fn edited_yaml_assignments_are_checked_and_can_reach_preview() {
        let mut project = decode_project_yaml(SAMPLE.into()).unwrap();
        project.plan.components.remove(0);
        project.plan.harness.id = "opencode".into();
        project.plan.harness.version = "2.2.1".into();
        let second_role = serde_json::from_value(serde_json::json!({
            "name": "Reviewer", "responsibility": "Review", "interaction": "Report",
            "reason": "Independent review", "modelId": "gpt-4-1"
        }))
        .unwrap();
        project.plan.roles.push(second_role);
        let saved = encode_project_yaml(project).unwrap();
        let reopened = decode_project_yaml(saved.clone()).unwrap();
        assert!(crate::preview::preview_configuration(
            reopened.plan,
            vec![],
            None,
            Default::default(),
            None,
        )
        .is_ok());
        let mut reopened = decode_project_yaml(saved).unwrap();
        reopened.plan.components.clear();
        let issues = crate::validation::validate_plan(reopened.plan, vec![]).unwrap();
        assert!(issues.iter().any(|issue| issue.contains("Developer")));
        assert!(issues.iter().any(|issue| issue.contains("Reviewer")));
    }

    #[test]
    fn restores_local_project_by_path_without_exporting_yaml() {
        let directory = std::env::temp_dir().join(Uuid::new_v4().to_string());
        let history = directory.join("history");
        let root = directory.join("project");
        fs::create_dir_all(&root).unwrap();
        let mut project = decode_project_yaml(SAMPLE.into()).unwrap();
        project.messages = Some(vec![ChatMessage {
            text: "Keep the existing stack".into(),
            role: ChatRole::User,
        }]);
        let id = Uuid::new_v4();
        save_history(&history, id, root.clone(), project).unwrap();
        let path = fs::canonicalize(&root).unwrap();
        let (entry, restored) = snapshot_for_path(&history, &path).unwrap().unwrap();
        assert_eq!(entry.id, id);
        assert_eq!(
            restored.messages.as_ref().unwrap()[0].text,
            "Keep the existing stack"
        );
        assert!(!root.join(PROJECT_FILE).exists());
        let exported = encode_project_yaml(restored).unwrap();
        let imported = decode_project_yaml(exported).unwrap();
        assert_eq!(imported.plan.components[0].id, "missing-tool");
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn history_survives_reopening_and_preserves_missing_snapshot_entry() {
        let directory = std::env::temp_dir().join(Uuid::new_v4().to_string());
        let history = directory.join("history");
        let first = directory.join("first");
        let second = directory.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        save_history(
            &history,
            a,
            first.clone(),
            decode_project_yaml(SAMPLE.into()).unwrap(),
        )
        .unwrap();
        save_history(
            &history,
            b,
            second.clone(),
            decode_project_yaml(SAMPLE.into()).unwrap(),
        )
        .unwrap();
        save_history(
            &history,
            a,
            first.clone(),
            decode_project_yaml(SAMPLE.into()).unwrap(),
        )
        .unwrap();
        assert_eq!(list_history(&history).unwrap().len(), 2);
        fs::remove_file(history.join(format!("{a}.yaml"))).unwrap();
        assert!(read_record(history.join(format!("{a}.yaml"))).is_err());
        assert!(list_history(&history)
            .unwrap()
            .iter()
            .any(|entry| entry.id == a));
        remove_history(&history, a).unwrap();
        remove_history(&history, a).unwrap();
        let remaining = list_history(&history).unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, b);
        assert!(first.is_dir());
        remove_history(&history, b).unwrap();
        assert!(!history.join(format!("{b}.yaml")).exists());
        assert!(second.is_dir());
        for path in [first, second, history, directory] {
            fs::remove_dir(path).unwrap();
        }
    }
}
