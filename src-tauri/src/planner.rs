use crate::{
    catalog::load_catalog,
    domain::{Budget, ComponentKind, Plan, ProjectProfile, RegistryEntry, Role, Selection},
    project::inspect_project,
};
use semver::{Version, VersionReq};
use serde::Serialize;
use std::path::PathBuf;

pub(crate) const TOKENS_PER_DAY: u64 = 100_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    plan: Plan,
    technologies: Vec<&'static str>,
    operation_cost: Option<f64>,
    planning_cost: f64,
    within_budget: Option<bool>,
    currency: String,
    period_days: u32,
    tokens: u64,
    notes: Vec<&'static str>,
    catalog_errors: Vec<String>,
}

pub(crate) fn cost(entry: &RegistryEntry, budget: &Budget, tokens: u64) -> Option<f64> {
    let model = entry.model.as_ref()?;
    if model.local {
        return Some(0.0);
    }
    let rate = model.rate.as_ref()?;
    if rate.currency != budget.currency {
        return None;
    }
    let days = u64::from(budget.period_days);
    let billable = |usage: u64, quota: Option<u64>| {
        usage.saturating_sub(quota.unwrap_or(0).saturating_mul(days))
    };
    let valid = |rate: f64| rate.is_finite() && rate >= 0.0;
    let amount = match (
        rate.input_per_token,
        rate.output_per_token,
        rate.per_million,
    ) {
        (Some(input), Some(output), _) if valid(input) && valid(output) => {
            let input_tokens = tokens - tokens / 5;
            let output_tokens = tokens - input_tokens;
            let input_quota = if input == 0.0 {
                0
            } else {
                rate.free_input_tokens_per_day?
            };
            let output_quota = if output == 0.0 {
                0
            } else {
                rate.free_output_tokens_per_day?
            };
            billable(input_tokens, Some(input_quota)) as f64 * input
                + billable(output_tokens, Some(output_quota)) as f64 * output
        }
        (None, None, Some(mixed)) if valid(mixed) => {
            billable(tokens, Some(rate.free_tokens_per_day?)) as f64 / 1_000_000.0 * mixed
        }
        _ => return None,
    };
    amount.is_finite().then_some(amount)
}

pub(crate) fn supports(harness: &RegistryEntry, entry: &RegistryEntry) -> bool {
    if entry.harness_versions.is_empty() {
        return true;
    }
    let Some(requirement) = entry.harness_versions.get(&harness.id) else {
        return false;
    };
    requirement == "*"
        || Version::parse(&harness.version)
            .ok()
            .zip(VersionReq::parse(requirement).ok())
            .is_some_and(|(version, range)| range.matches(&version))
}

fn can_add(harness: &RegistryEntry, chosen: &[&RegistryEntry], entry: &RegistryEntry) -> bool {
    supports(harness, entry)
        && entry
            .requires
            .iter()
            .all(|id| chosen.iter().any(|item| item.id == *id))
        && chosen
            .iter()
            .all(|item| !item.conflicts.contains(&entry.id) && !entry.conflicts.contains(&item.id))
}

fn selection(entry: &RegistryEntry, reason: impl Into<String>) -> Selection {
    Selection {
        id: entry.id.clone(),
        version: entry.version.clone(),
        reason: reason.into(),
    }
}

fn rank(cost: Option<f64>, budget: f64, quality: u8, priority: u8) -> (u8, f64) {
    let tier = match cost {
        Some(amount) if amount <= budget => 2,
        None => 1,
        Some(_) => 0,
    };
    let economy = cost.map_or(0.0, |amount| 1.0 / (1.0 + amount));
    let quality_weight = f64::from(priority) / 100.0;
    (
        tier,
        economy * (1.0 - quality_weight) + f64::from(quality.min(100)) / 100.0 * quality_weight,
    )
}

#[tauri::command]
pub fn recommend(
    profile: ProjectProfile,
    extension_paths: Vec<PathBuf>,
) -> Result<Recommendation, String> {
    let context = inspect_project(profile)?;
    let profile = context.profile;
    let catalog = load_catalog(extension_paths)?;
    let tokens = TOKENS_PER_DAY * u64::from(profile.budget.period_days);
    let project_text = format!(
        "{} {}",
        profile.idea.as_deref().unwrap_or_default(),
        context.technologies.join(" ")
    )
    .to_lowercase();
    let available = |entry: &RegistryEntry| {
        entry.model.as_ref().is_some_and(|model| {
            profile.providers.iter().any(|id| {
                id.eq_ignore_ascii_case(&entry.id) || id.eq_ignore_ascii_case(&model.provider)
            })
        })
    };
    let relevant = |entry: &RegistryEntry| {
        entry
            .project_tags
            .iter()
            .any(|tag| !tag.trim().is_empty() && project_text.contains(&tag.to_lowercase()))
    };
    let mut best = None;
    for harness in catalog
        .entries
        .iter()
        .filter(|entry| entry.kind == ComponentKind::Harness && entry.requires.is_empty())
    {
        let mut chosen = vec![harness];
        let mut components = Vec::new();
        loop {
            let previous = chosen.len();
            for entry in &catalog.entries {
                if matches!(entry.kind, ComponentKind::Harness | ComponentKind::Model)
                    || !relevant(entry)
                    || chosen.iter().any(|item| item.id == entry.id)
                    || !can_add(harness, &chosen, entry)
                {
                    continue;
                }
                components.push(selection(
                    entry,
                    "Matches the project signals and works with the selected harness.",
                ));
                chosen.push(entry);
            }
            if chosen.len() == previous {
                break;
            }
        }
        let model = catalog
            .entries
            .iter()
            .filter(|entry| {
                entry.kind == ComponentKind::Model
                    && available(entry)
                    && can_add(harness, &chosen, entry)
            })
            .map(|entry| {
                let amount = cost(entry, &profile.budget, tokens);
                let score = rank(
                    amount,
                    profile.budget.amount,
                    entry.model.as_ref().map_or(0, |model| model.quality),
                    profile.priority,
                );
                (entry, amount, score)
            })
            .max_by(|a, b| a.2 .0.cmp(&b.2 .0).then_with(|| a.2 .1.total_cmp(&b.2 .1)));
        let score = (
            model.as_ref().map_or(0, |item| item.2 .0 + 1),
            model.as_ref().map_or(0.0, |item| item.2 .1),
            components.len(),
        );
        if best
            .as_ref()
            .is_none_or(|(previous, _, _, _)| score > *previous)
        {
            best = Some((score, harness, components, model));
        }
    }
    let (_, harness, mut components, model) = best.ok_or("The catalog has no suitable harness.")?;
    let operation_cost = model.as_ref().and_then(|(_, amount, _)| *amount);
    if let Some((entry, _, _)) = model {
        components.push(selection(
            entry,
            "Available in the selected environment. The selection accounts for budget, quota, and priority.",
        ));
    }
    let mut notes = vec![
        "This estimate uses 100,000 tokens per day at a blended rate. Local compute excludes hardware and electricity costs.",
        "Local rule-based planning is free. The budget is compared with estimated running costs.",
    ];
    if context.technologies.is_empty() {
        notes.push("The plan uses the project description and catalog data. No recognized manifest confirmed the project technologies.");
    }
    if operation_cost.is_none() {
        notes.push(
            "Cost is unknown because a compatible model, rate, or quota in the budget currency is unavailable.",
        );
    }
    Ok(Recommendation {
        plan: Plan {
            harness: selection(harness, "Supports the available configuration and suitable project components."),
            components,
            roles: vec![Role {
                model_id: None,
                parallel_with: Vec::new(),
                name: "Developer".into(),
                responsibility: "Implement the task and verify the result.".into(),
                interaction: "Confirm requirement changes with the user.".into(),
                reason: "One role covers implementation and verification when there are no separate responsibilities.".into(),
            }],
            instructions: vec!["Follow the project rules and verify the result before finishing the task.".into()],
        },
        technologies: context.technologies,
        planning_cost: 0.0,
        within_budget: operation_cost.map(|amount| amount <= profile.budget.amount),
        operation_cost,
        currency: profile.budget.currency,
        period_days: profile.budget.period_days,
        tokens,
        notes,
        catalog_errors: catalog.errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TokenRate;

    fn profile() -> ProjectProfile {
        ProjectProfile {
            idea: Some("A small CLI with OpenSpec".into()),
            repository: None,
            providers: vec!["ollama".into()],
            budget: Budget {
                amount: 0.0,
                currency: "USD".into(),
                period_days: 2,
            },
            priority: 0,
        }
    }

    #[test]
    fn recommends_one_role_and_uses_only_operation_cost_for_budget() {
        let result = recommend(profile(), vec![]).unwrap();
        assert_eq!(result.plan.roles.len(), 1);
        assert_eq!(result.plan.instructions.len(), 1);
        assert_eq!(result.within_budget, Some(true));
        assert_eq!(result.operation_cost, Some(0.0));
        assert!(result
            .plan
            .components
            .iter()
            .any(|item| item.id == "openspec"));
        assert!(result
            .plan
            .components
            .iter()
            .any(|item| item.id == "qwen2-5-coder"));
        let mut cloud_profile = profile();
        cloud_profile.providers = vec!["openai".into()];
        let cloud = recommend(cloud_profile, vec![]).unwrap();
        assert_eq!(cloud.operation_cost, None);
        assert_eq!(cloud.within_budget, None);
    }

    #[test]
    fn discounts_free_quota_and_respects_priority_and_budget() {
        let mut catalog = load_catalog(vec![]).unwrap();
        let model = catalog
            .entries
            .iter_mut()
            .find(|item| item.id == "gpt-4-1")
            .unwrap();
        let rate = &mut model.model.as_mut().unwrap().rate;
        *rate = Some(TokenRate {
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
        let amount = cost(model, &profile().budget, 200_000);
        assert_eq!(amount, Some(1.0));
        assert!(rank(Some(0.0), 5.0, 55, 0) > rank(amount, 5.0, 85, 0));
        assert!(rank(Some(0.0), 5.0, 55, 100) < rank(amount, 5.0, 85, 100));
        assert!(rank(Some(0.0), 0.0, 55, 100) > rank(amount, 0.0, 85, 100));
        model
            .model
            .as_mut()
            .unwrap()
            .rate
            .as_mut()
            .unwrap()
            .free_tokens_per_day = None;
        assert_eq!(cost(model, &profile().budget, 200_000), None);
    }
}
