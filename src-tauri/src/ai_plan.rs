use crate::{
    catalog::{load_catalog, Catalog},
    domain::{validate_profile, ComponentKind, Plan, ProjectProfile, Role, Selection},
    planner::{cost, TOKENS_PER_DAY},
    validation::check,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[tauri::command]
pub fn evaluate_ai_plan(
    candidate: AiPlan,
    profile: ProjectProfile,
    extension_paths: Vec<PathBuf>,
) -> Result<OperationEstimate, String> {
    let profile = validate_profile(profile)?;
    let catalog = load_catalog(extension_paths)?;
    candidate.validate_and_estimate(&profile, &catalog)
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiPlan {
    pub plan: Plan,
    pub assumptions: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PlanPatch {
    operations: Vec<PlanOperation>,
    assumptions: Option<Vec<String>>,
    pub(crate) project_brief: Option<String>,
    pub(crate) session_title: Option<String>,
    pub(crate) hook_checks: Option<Vec<PlannedHook>>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PlannedHook {
    pub(crate) title: String,
    pub(crate) description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) check: Option<HookCheck>,
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HookCheck {
    Test,
    Lint,
    Typecheck,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum PlanOperation {
    SetHarness {
        id: String,
        reason: Option<String>,
    },
    AddComponent {
        id: String,
        reason: Option<String>,
    },
    RemoveComponent {
        id: String,
    },
    AddRole {
        role: Role,
    },
    UpdateRole {
        name: String,
        #[serde(rename = "newName")]
        new_name: Option<String>,
        responsibility: Option<String>,
        interaction: Option<String>,
        reason: Option<String>,
        #[serde(rename = "modelId")]
        model_id: Option<String>,
        #[serde(rename = "parallelWith")]
        parallel_with: Option<Vec<String>>,
    },
    RemoveRole {
        name: String,
    },
    AddInstruction {
        text: String,
    },
    RemoveInstruction {
        text: String,
    },
    SetDependency {
        dependency: crate::domain::ProjectDependency,
    },
    RemoveDependency {
        ecosystem: String,
        package: String,
    },
}

impl PlanPatch {
    pub(crate) fn apply(self, mut current: AiPlan, catalog: &Catalog) -> Result<AiPlan, String> {
        for (index, operation) in self.operations.into_iter().enumerate() {
            apply_operation(&mut current.plan, operation, catalog)
                .map_err(|error| format!("Operation {}: {error}", index + 1))?;
        }
        if let Some(assumptions) = self.assumptions {
            current.assumptions = assumptions;
        }
        Ok(current)
    }
}

fn selected_entry(
    catalog: &Catalog,
    id: String,
    reason: Option<String>,
    harness: bool,
) -> Result<Selection, String> {
    let entry = catalog
        .entries
        .iter()
        .find(|entry| entry.id == id)
        .ok_or_else(|| format!("Unknown catalog ID: {id}."))?;
    if (entry.kind == ComponentKind::Harness) != harness {
        return Err(format!("{id} belongs to a different layer."));
    }
    Ok(Selection {
        id,
        version: entry.version.clone(),
        reason: reason
            .filter(|text| !text.trim().is_empty())
            .or_else(|| entry.purpose.clone())
            .unwrap_or_else(|| "Selected for the project.".into()),
    })
}

fn apply_operation(
    plan: &mut Plan,
    operation: PlanOperation,
    catalog: &Catalog,
) -> Result<(), String> {
    match operation {
        PlanOperation::SetDependency { mut dependency } => {
            dependency.manifest = None;
            if let Some(current) = plan.dependencies.iter_mut().find(|item| {
                item.ecosystem == dependency.ecosystem && item.package == dependency.package
            }) {
                dependency.manifest = current.manifest.clone();
                *current = dependency;
            } else {
                plan.dependencies.push(dependency);
            }
        }
        PlanOperation::RemoveDependency { ecosystem, package } => {
            let index = plan
                .dependencies
                .iter()
                .position(|item| item.ecosystem == ecosystem && item.package == package)
                .ok_or("The project dependency is not in the plan.")?;
            plan.dependencies.remove(index);
        }
        PlanOperation::SetHarness { id, reason } => {
            plan.harness = selected_entry(catalog, id, reason, true)?
        }
        PlanOperation::AddComponent { id, reason } => {
            if plan.components.iter().any(|component| component.id == id) {
                return Err(format!("{id} is already selected."));
            }
            plan.components
                .push(selected_entry(catalog, id, reason, false)?);
        }
        PlanOperation::RemoveComponent { id } => {
            let index = plan
                .components
                .iter()
                .position(|component| component.id == id)
                .ok_or_else(|| format!("{id} is not selected."))?;
            plan.components.remove(index);
        }
        PlanOperation::AddRole { role } => plan.roles.push(role),
        PlanOperation::UpdateRole {
            name,
            new_name,
            responsibility,
            interaction,
            reason,
            model_id,
            parallel_with,
        } => {
            let role = plan
                .roles
                .iter_mut()
                .find(|role| role.name == name)
                .ok_or_else(|| format!("Unknown role: {name}."))?;
            if let Some(value) = &new_name {
                role.name = value.clone();
            }
            if let Some(value) = responsibility {
                role.responsibility = value;
            }
            if let Some(value) = interaction {
                role.interaction = value;
            }
            if let Some(value) = reason {
                role.reason = value;
            }
            if let Some(value) = parallel_with {
                role.parallel_with = value;
            }
            if let Some(value) = model_id {
                role.model_id = if value.is_empty() { None } else { Some(value) };
            }
            if let Some(new_name) = new_name {
                for role in &mut plan.roles {
                    for peer in &mut role.parallel_with {
                        if *peer == name {
                            *peer = new_name.clone();
                        }
                    }
                }
            }
        }
        PlanOperation::RemoveRole { name } => {
            let index = plan
                .roles
                .iter()
                .position(|role| role.name == name)
                .ok_or_else(|| format!("Unknown role: {name}."))?;
            plan.roles.remove(index);
            for role in &mut plan.roles {
                role.parallel_with.retain(|peer| peer != &name);
            }
        }
        PlanOperation::AddInstruction { text } => plan.instructions.push(text),
        PlanOperation::RemoveInstruction { text } => {
            let index = plan
                .instructions
                .iter()
                .position(|instruction| instruction == &text)
                .ok_or("The instruction does not exist.")?;
            plan.instructions.remove(index);
        }
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationEstimate {
    pub amount: Option<f64>,
    pub within_budget: Option<bool>,
    pub tokens_per_model: u64,
}

impl AiPlan {
    pub(crate) fn validate_and_estimate(
        &self,
        profile: &ProjectProfile,
        catalog: &Catalog,
    ) -> Result<OperationEstimate, String> {
        let mut issues = check(&self.plan, &catalog.entries);
        if std::iter::once(&self.plan.harness)
            .chain(&self.plan.components)
            .any(|entry| entry.reason.trim().is_empty())
            || self.plan.roles.iter().any(|role| {
                [
                    &role.name,
                    &role.reason,
                    &role.interaction,
                    &role.responsibility,
                ]
                .iter()
                .any(|value| value.trim().is_empty())
            })
        {
            issues.push("Explain the component choices and describe each role in the plan.".into());
        }
        let models = self.plan.components.iter().filter_map(|item| {
            catalog
                .entries
                .iter()
                .find(|entry| entry.id == item.id && entry.kind == ComponentKind::Model)
        });
        let tokens_per_model = TOKENS_PER_DAY * u64::from(profile.budget.period_days);
        let mut amount: Option<f64> = None;
        let mut count = 0;
        for entry in models {
            if !profile.providers.is_empty()
                && !entry.model.as_ref().is_some_and(|model| {
                    profile.providers.iter().any(|choice| {
                        choice.eq_ignore_ascii_case(&entry.id)
                            || choice.eq_ignore_ascii_case(&model.provider)
                    })
                })
            {
                issues.push(format!(
                    "Project model {} is outside the configured provider and model list.",
                    entry.id
                ));
            }
            let model_cost = cost(entry, &profile.budget, tokens_per_model);
            amount = if count == 0 {
                model_cost
            } else {
                amount.zip(model_cost).map(|(total, next)| total + next)
            };
            count += 1;
        }
        if !issues.is_empty() {
            return Err(issues.join("\n"));
        }
        let amount = amount.filter(|value| value.is_finite());
        Ok(OperationEstimate {
            amount,
            within_budget: amount.map(|amount| amount <= profile.budget.amount),
            tokens_per_model,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TokenRate;
    use serde_json::json;

    #[test]
    fn patch_updates_only_requested_fields_and_reports_invalid_operations() {
        let source = json!({
            "plan": {
                "harness": {"id":"codex","version":"0.167.1","reason":"Existing harness"},
                "components": [{"id":"typescript","version":"unknown","reason":"Existing language"}],
                "roles": [{"name":"Developer","responsibility":"Build","interaction":"Report","reason":"Primary"}],
                "instructions": ["Run tests"]
            },
            "assumptions": ["Preserve this"]
        });
        let catalog = load_catalog(vec![]).unwrap();
        let patch: PlanPatch = serde_json::from_value(json!({"operations": [
            {"op":"addComponent","id":"nginx"},
            {"op":"updateRole","name":"Developer","interaction":"Report daily"}
        ]}))
        .unwrap();
        let changed = patch
            .apply(serde_json::from_value(source.clone()).unwrap(), &catalog)
            .unwrap();
        assert_eq!(changed.plan.components[0].id, "typescript");
        assert_eq!(changed.plan.components[1].id, "nginx");
        assert_eq!(changed.plan.harness.reason, "Existing harness");
        assert_eq!(changed.plan.roles[0].responsibility, "Build");
        assert_eq!(changed.plan.roles[0].interaction, "Report daily");
        assert_eq!(changed.plan.instructions, ["Run tests"]);
        assert_eq!(changed.assumptions, ["Preserve this"]);
        let invalid: PlanPatch = serde_json::from_value(json!({"operations": [
            {"op":"addComponent","id":"nginx"}, {"op":"addComponent","id":"unknown-id"}
        ]}))
        .unwrap();
        let error = invalid
            .apply(serde_json::from_value(source).unwrap(), &catalog)
            .err()
            .unwrap();
        assert!(error.contains("Operation 2: Unknown catalog ID"));
    }

    #[test]
    fn validates_model_access_and_calculates_budget_from_catalog() {
        let mut profile: ProjectProfile = serde_json::from_value(json!({
            "idea": "A CLI", "providers": ["openai"], "budget": {
                "amount": 0.5, "currency": "USD", "periodDays": 2
            }, "priority": 50
        }))
        .unwrap();
        let candidate: AiPlan = serde_json::from_value(json!({
            "plan": {
                "harness": {"id": "opencode", "version": "2.2.1", "reason": "Supports the model"},
                "components": [{"id": "gpt-4-1", "version": "gpt-4.1-2025-04-14", "reason": "Available"}],
                "roles": [{"name": "Developer", "responsibility": "Implement", "interaction": "Report", "reason": "One role is enough"}],
                "instructions": ["Run tests"]
            },
            "assumptions": ["Small project"]
        })).unwrap();
        let mut catalog = load_catalog(vec![]).unwrap();
        assert_eq!(
            candidate
                .validate_and_estimate(&profile, &catalog)
                .unwrap()
                .within_budget,
            None
        );
        let model = catalog
            .entries
            .iter_mut()
            .find(|entry| entry.id == "gpt-4-1")
            .unwrap();
        model.model.as_mut().unwrap().rate = Some(TokenRate {
            currency: "USD".into(),
            per_million: Some(20.0),
            free_tokens_per_day: Some(75_000),
            input_per_token: None,
            output_per_token: None,
            free_input_tokens_per_day: None,
            free_output_tokens_per_day: None,
            source: None,
            checked_on: None,
        });
        let estimate = candidate.validate_and_estimate(&profile, &catalog).unwrap();
        assert_eq!(estimate.amount, Some(1.0));
        assert_eq!(estimate.within_budget, Some(false));
        profile.providers.clear();
        assert!(candidate.validate_and_estimate(&profile, &catalog).is_ok());
        profile.providers.push("ollama".into());
        assert!(candidate.validate_and_estimate(&profile, &catalog).is_err());
    }
}
