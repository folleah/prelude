use crate::{
    catalog::load_catalog,
    domain::{ComponentKind, Plan, RegistryEntry},
    planner::supports,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[tauri::command]
pub fn validate_plan(plan: Plan, extension_paths: Vec<PathBuf>) -> Result<Vec<String>, String> {
    let catalog = load_catalog(extension_paths)?;
    let mut issues = catalog.errors;
    issues.extend(check(&plan, &catalog.entries));
    Ok(issues)
}

pub(crate) fn check(plan: &Plan, entries: &[RegistryEntry]) -> Vec<String> {
    let catalog: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let mut issues = Vec::new();
    let mut ids = BTreeSet::new();
    let harness = catalog
        .get(plan.harness.id.as_str())
        .copied()
        .filter(|entry| entry.kind == ComponentKind::Harness);
    if harness.is_none() {
        issues.push(format!(
            "Choose a harness from the catalog instead of {}.",
            plan.harness.id
        ));
    }
    for item in std::iter::once(&plan.harness).chain(&plan.components) {
        if !ids.insert(item.id.as_str()) {
            issues.push(format!("Remove the duplicate component {}.", item.id));
        }
    }
    for item in std::iter::once(&plan.harness).chain(&plan.components) {
        let Some(entry) = catalog.get(item.id.as_str()).copied() else {
            issues.push(format!(
                "Add {} to the catalog or remove it from the plan.",
                item.id
            ));
            continue;
        };
        if item.version != entry.version {
            issues.push(format!(
                "For {}, select version {} from the catalog.",
                item.id, entry.version
            ));
        }
        if item.id != plan.harness.id
            && (entry.kind == ComponentKind::Harness
                || harness.is_some_and(|h| !supports(h, entry)))
        {
            issues.push(format!(
                "{} is incompatible with {}. Replace the component or the harness.",
                item.id, plan.harness.id
            ));
        }
        for required in &entry.requires {
            if !ids.contains(required.as_str()) {
                issues.push(format!(
                    "For {}, add {} or remove the dependent component.",
                    item.id, required
                ));
            }
        }
        for conflict in &entry.conflicts {
            if ids.contains(conflict.as_str()) {
                issues.push(format!(
                    "{} and {} conflict. Remove one of the components.",
                    item.id, conflict
                ));
            }
        }
    }
    if plan.roles.is_empty() {
        issues.push("Add at least one role.".into());
    }
    let mut names = BTreeSet::new();
    for role in &plan.roles {
        let mut peers = BTreeSet::new();
        for peer in &role.parallel_with {
            if peer == &role.name
                || !peers.insert(peer)
                || !plan.roles.iter().any(|other| &other.name == peer)
            {
                issues.push(format!(
                    "Invalid parallel role reference for {}: {peer}.",
                    role.name
                ));
            }
        }
        if let Some(model_id) = &role.model_id {
            if !plan.components.iter().any(|item| item.id == *model_id)
                || !entries
                    .iter()
                    .any(|entry| entry.id == *model_id && entry.kind == ComponentKind::Model)
            {
                issues.push(format!(
                    "Choose a model from the project for role {}.",
                    role.name
                ));
            }
        }
        if role.name.trim().is_empty() || !names.insert(role.name.trim().to_lowercase()) {
            issues.push(format!(
                "Enter a unique, nonempty role name instead of \"{}\".",
                role.name
            ));
        }
    }
    let mut instructions = BTreeSet::new();
    for instruction in &plan.instructions {
        let normalized = instruction
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        if normalized.is_empty() || !instructions.insert(normalized) {
            issues.push(format!(
                "Remove the empty or duplicate instruction \"{}\".",
                instruction
            ));
        }
    }
    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn updates_validation_when_conflict_is_removed() {
        let mut catalog = load_catalog(vec![]).unwrap();
        catalog
            .entries
            .iter_mut()
            .find(|item| item.id == "openspec")
            .unwrap()
            .conflicts
            .push("ponytail".into());
        let mut plan = serde_json::from_value(serde_json::json!({
            "harness": { "id": "codex", "version": "0.167.1", "reason": "" },
            "components": [
                { "id": "openspec", "version": "unknown", "reason": "" },
                { "id": "ponytail", "version": "unknown", "reason": "" }
            ],
            "roles": [{ "name": "Developer", "responsibility": "", "interaction": "", "reason": "" }],
            "instructions": ["Run tests"]
        })).unwrap();
        assert_eq!(check(&plan, &catalog.entries).len(), 1);
        plan.components.pop();
        assert!(check(&plan, &catalog.entries).is_empty());
    }
}
