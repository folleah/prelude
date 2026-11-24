use crate::{
    catalog::{load_catalog, Catalog},
    domain::{ComponentKind, Plan},
    validation::check,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Mutex, time::Duration};

const MAX_RESPONSE: usize = 64 * 1024;
const SYSTEM: &str = "Review the proposed coding environment. Identify optional items that are unnecessary for a generic software project. Return only a JSON object with one field remove, an array of zero-based optional item indices. Keep unfamiliar items. Preserve dependencies.";

#[derive(Default)]
pub struct SessionKey(Mutex<Option<reqwest::header::HeaderValue>>);

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Ollama,
    Openai,
}

impl Provider {
    fn target(self) -> (&'static str, &'static str) {
        match self {
            Self::Ollama => (
                "http://127.0.0.1:11434/v1/chat/completions",
                "qwen2.5-coder:7b",
            ),
            Self::Openai => (
                "https://api.openai.com/v1/chat/completions",
                "gpt-4.1-2025-04-14",
            ),
        }
    }
}

#[derive(Serialize)]
pub struct RefinementPreview {
    destination: &'static str,
    body: String,
}

#[tauri::command]
pub fn set_cloud_key(key: String, state: tauri::State<'_, SessionKey>) -> Result<(), String> {
    state.replace(&key)
}

impl SessionKey {
    fn replace(&self, key: &str) -> Result<(), String> {
        let mut stored = self
            .0
            .lock()
            .map_err(|_| "Session storage is unavailable.")?;
        let key = key.trim();
        *stored = if key.is_empty() {
            None
        } else {
            let mut header = reqwest::header::HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| "The key contains invalid characters.")?;
            header.set_sensitive(true);
            Some(header)
        };
        Ok(())
    }
}

fn require_confirmation(
    preview: &RefinementPreview,
    confirmed_body: Option<&str>,
) -> Result<(), String> {
    if confirmed_body == Some(preview.body.as_str()) {
        Ok(())
    } else {
        Err("Confirm the current request contents.".into())
    }
}

fn context(
    plan: &Plan,
    catalog: &Catalog,
    provider: Provider,
) -> Result<(RefinementPreview, Vec<usize>), String> {
    if !catalog.errors.is_empty() || !check(plan, &catalog.entries).is_empty() {
        return Err("Fix the plan errors first.".into());
    }
    let optional: Vec<_> = plan
        .components
        .iter()
        .enumerate()
        .filter(|(_, item)| {
            catalog
                .entries
                .iter()
                .any(|entry| entry.id == item.id && entry.kind != ComponentKind::Model)
        })
        .collect();
    let preferences = crate::module_preferences::current();
    let labels: Vec<_> = optional
        .iter()
        .map(|(_, item)| {
            preferences
                .priorities
                .get(&item.id)
                .and_then(|rule| rule.refinement_label.as_deref())
                .unwrap_or("Custom component")
        })
        .collect();
    let harness = match plan.harness.id.as_str() {
        "codex" => "Codex",
        "claude-code" => "Claude Code",
        "opencode" => "OpenCode",
        _ => "Custom harness",
    };
    let (destination, model) = provider.target();
    let body = json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": json!({"harness": harness, "optionalItems": labels}).to_string()}
        ],
        "response_format": {"type": "json_object"},
        "stream": false
    }).to_string();
    Ok((
        RefinementPreview { destination, body },
        optional.into_iter().map(|(index, _)| index).collect(),
    ))
}

#[tauri::command]
pub fn preview_refinement(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
    provider: Provider,
) -> Result<RefinementPreview, String> {
    let catalog = load_catalog(extension_paths)?;
    context(&plan, &catalog, provider).map(|(preview, _)| preview)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    remove: Vec<usize>,
}

fn apply_response(plan: &mut Plan, optional: &[usize], response: &[u8]) -> Result<(), String> {
    let result: Value =
        serde_json::from_slice(response).map_err(|_| "The model returned an invalid response.")?;
    let content = result
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or("The model response does not contain a proposal.")?;
    let change: Change =
        serde_json::from_str(content).map_err(|_| "The model returned an invalid proposal.")?;
    let mut indices = std::collections::BTreeSet::new();
    for index in change.remove {
        let index = optional
            .get(index)
            .ok_or("The model specified an unknown component.")?;
        if !indices.insert(*index) {
            return Err("The model included a duplicate component in the proposal.".into());
        }
    }
    for index in indices.into_iter().rev() {
        plan.components.remove(index);
    }
    Ok(())
}

#[tauri::command]
pub async fn refine_plan(
    mut plan: Plan,
    extension_paths: Vec<PathBuf>,
    provider: Provider,
    confirmed_body: Option<String>,
    state: tauri::State<'_, SessionKey>,
) -> Result<Plan, String> {
    let catalog = load_catalog(extension_paths)?;
    let (preview, optional) = context(&plan, &catalog, provider)?;
    require_confirmation(&preview, confirmed_body.as_deref())?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client
        .post(preview.destination)
        .header(reqwest::header::CONTENT_TYPE, "application/json");
    if matches!(provider, Provider::Openai) {
        let key = state
            .0
            .lock()
            .map_err(|_| "Session storage is unavailable.")?
            .clone()
            .ok_or("Enter an OpenAI API key.")?;
        request = request.header(reqwest::header::AUTHORIZATION, key);
    }
    let mut response = request
        .body(preview.body)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_RESPONSE {
            return Err("The model response exceeds the size limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    apply_response(&mut plan, &optional, &bytes)?;
    let issues = check(&plan, &catalog.entries);
    if !issues.is_empty() {
        return Err(format!(
            "The model proposed an incompatible plan. {}",
            issues.join(" ")
        ));
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_can_be_replaced_and_cleared() {
        let key = SessionKey::default();
        key.replace("old").unwrap();
        key.replace("new").unwrap();
        let stored = key.0.lock().unwrap().clone().unwrap();
        assert_eq!(stored, "Bearer new");
        assert!(stored.is_sensitive());
        key.replace("").unwrap();
        assert!(key.0.lock().unwrap().is_none());
    }

    #[test]
    fn preview_omits_project_text_and_requires_consent() {
        let mut plan: Plan = serde_json::from_value(json!({
            "harness": { "id": "codex", "version": "0.167.1", "reason": "test-secret" },
            "components": [{ "id": "openspec", "version": "unknown", "reason": "" }],
            "roles": [{ "name": "Developer", "responsibility": "", "interaction": "", "reason": "" }],
            "instructions": ["test-secret"]
        })).unwrap();
        let catalog = load_catalog(vec![]).unwrap();
        let (preview, optional) = context(&plan, &catalog, Provider::Ollama).unwrap();
        assert!(preview.destination.starts_with("http://127.0.0.1:"));
        assert!(!preview.body.contains("test-secret"));
        assert!(require_confirmation(&preview, None).is_err());
        assert!(require_confirmation(&preview, Some(&preview.body)).is_ok());
        assert!(apply_response(&mut plan, &optional, b"invalid").is_err());
        let invalid = json!({ "choices": [{ "message": { "content": "{\"remove\":[99]}" } }] });
        assert!(
            apply_response(&mut plan, &optional, &serde_json::to_vec(&invalid).unwrap()).is_err()
        );
        assert_eq!(plan.components.len(), 1);
        let valid = json!({ "choices": [{ "message": { "content": "{\"remove\":[0]}" } }] });
        apply_response(&mut plan, &optional, &serde_json::to_vec(&valid).unwrap()).unwrap();
        assert!(plan.components.is_empty());
    }
}
