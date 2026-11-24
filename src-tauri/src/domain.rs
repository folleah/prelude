use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectProfile {
    pub idea: Option<String>,
    pub repository: Option<String>,
    #[serde(default)]
    pub providers: Vec<String>,
    pub budget: Budget,
    pub priority: u8,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Budget {
    pub amount: f64,
    pub currency: String,
    pub period_days: u32,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Harness,
    Model,
    Skill,
    Mcp,
    Tool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistryEntry {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub kind: ComponentKind,
    pub source: String,
    pub version: String,
    pub requires: Vec<String>,
    pub conflicts: Vec<String>,
    pub harness_versions: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_docs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<ComponentDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<Model>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_tags: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ComponentDelivery {
    Skill {
        description: String,
        instructions: String,
    },
    RemoteMcp {
        url: String,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Model {
    pub provider: String,
    pub quality: u8,
    pub local: bool,
    pub rate: Option<TokenRate>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenRate {
    pub currency: String,
    pub per_million: Option<f64>,
    pub free_tokens_per_day: Option<u64>,
    pub input_per_token: Option<f64>,
    pub output_per_token: Option<f64>,
    pub free_input_tokens_per_day: Option<u64>,
    pub free_output_tokens_per_day: Option<u64>,
    pub source: Option<String>,
    pub checked_on: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    pub harness: Selection,
    pub components: Vec<Selection>,
    pub roles: Vec<Role>,
    pub instructions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<ProjectDependency>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDependency {
    pub ecosystem: String,
    pub package: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Selection {
    pub id: String,
    pub version: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Role {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parallel_with: Vec<String>,
    pub name: String,
    pub responsibility: String,
    pub interaction: String,
    pub reason: String,
}

#[tauri::command]
pub fn validate_profile(profile: ProjectProfile) -> Result<ProjectProfile, String> {
    let has_text =
        |value: &Option<String>| value.as_deref().is_some_and(|text| !text.trim().is_empty());
    if !has_text(&profile.idea) && !has_text(&profile.repository) {
        return Err("Enter a project idea or a local repository.".into());
    }

    let budget = &profile.budget;
    if !budget.amount.is_finite() || budget.amount < 0.0 || budget.period_days == 0 {
        return Err("Enter a nonnegative budget and a period in days.".into());
    }
    if budget.currency.len() != 3 || !budget.currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err("Enter a three-letter currency code such as USD.".into());
    }
    if profile.priority > 100 {
        return Err("Priority must be between 0 and 100.".into());
    }
    Ok(profile)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_typed_payload_and_rejects_invalid_priority() {
        let mut payload = serde_json::json!({
            "idea": "Desktop application",
            "providers": [],
            "budget": {"amount": 0, "currency": "USD", "periodDays": 30},
            "priority": 50
        });
        assert!(validate_profile(serde_json::from_value(payload.clone()).unwrap()).is_ok());
        payload["priority"] = 101.into();
        assert!(validate_profile(serde_json::from_value(payload.clone()).unwrap()).is_err());
        payload["priority"] = "50".into();
        assert!(serde_json::from_value::<ProjectProfile>(payload).is_err());
    }
}
