use super::*;

const SCENARIOS: &str = include_str!("../tests/fixtures/planning/scenarios.yaml");
const BASELINE: &str = concat!(
    include_str!("../tests/fixtures/planning/baseline-planning.md"),
    "\n",
    include_str!("../tests/fixtures/planning/baseline-format.md")
);
const CANDIDATE: &str = concat!(
    include_str!("planning-prompt.md"),
    "\n",
    include_str!("prelude-format.md")
);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Scenario {
    id: String,
    idea: String,
    #[serde(default)]
    discovery_answers: Vec<[String; 2]>,
    criteria: Vec<String>,
}

fn scenarios() -> Vec<Scenario> {
    serde_yaml::from_str(SCENARIOS).expect("valid evaluation scenarios")
}

fn evaluation_messages(system: &str, input: &str, catalog: &str) -> Vec<Value> {
    vec![
        json!({"role": "system", "content": format!("{system}\nThe local catalog is supplied below in the input. Use it directly in place of all list_catalog calls. Do not call tools.")}),
        json!({"role": "user", "content": input}),
        json!({"role": "user", "content": format!("Local catalog data:\n{catalog}")}),
    ]
}

#[test]
fn planning_evaluation_pairs_differ_only_in_system_prompt() {
    let scenarios = scenarios();
    let ids: std::collections::BTreeSet<_> = scenarios.iter().map(|case| &case.id).collect();
    assert_eq!(ids.len(), 5);
    assert!(scenarios
        .iter()
        .all(|case| !case.idea.is_empty() && !case.criteria.is_empty()));
    for case in scenarios {
        let baseline = evaluation_messages(BASELINE, &case.idea, "catalog snapshot");
        let candidate = evaluation_messages(CANDIDATE, &case.idea, "catalog snapshot");
        assert_eq!(baseline[1], candidate[1]);
        assert_eq!(baseline[2], candidate[2]);
    }
}

#[test]
fn planning_evaluation_uses_real_patch_validation_for_targeted_edits() -> Result<(), String> {
    let catalog = load_catalog(vec![])?;
    let empty: AiPlan = serde_json::from_value(json!({
        "plan":{"harness":{"id":"","version":"","reason":""}, "components":[], "roles":[], "instructions":[]},
        "assumptions":[]
    })).map_err(|error| error.to_string())?;
    let generated = parse_plan_patch_for_catalog(
        "operations:\n  - { op: setHarness, id: codex }\n  - { op: addComponent, id: git }\n  - op: addRole\n    role: { name: Developer, responsibility: Render an article, interaction: Check the output, reason: One workflow }\n  - { op: addInstruction, text: Keep articles in Markdown. }\nhookChecks:\n  - { title: Run tests, description: After editing the renderer run its fast tests. }",
        &catalog,
    )?;
    let hooks = generated.hook_checks.as_ref().expect("initial hooks");
    assert_eq!(hooks.len(), 1);
    let current = generated.apply(empty, &catalog)?;
    let before = serde_json::to_value(&current.plan).map_err(|error| error.to_string())?;
    let edit = parse_plan_patch_for_catalog(
        "operations:\n  - { op: addInstruction, text: Add a check for broken links. }",
        &catalog,
    )?;
    assert!(
        edit.hook_checks.is_none(),
        "an omitted hook list preserves current hooks"
    );
    let updated = edit.apply(current, &catalog)?;
    assert!(crate::validation::check(&updated.plan, &catalog.entries).is_empty());
    let after = serde_json::to_value(&updated.plan).map_err(|error| error.to_string())?;
    for key in ["roles", "components", "harness"] {
        assert_eq!(after[key], before[key]);
    }
    assert_eq!(
        after["instructions"],
        json!([
            "Keep articles in Markdown.",
            "Add a check for broken links."
        ])
    );
    Ok(())
}

#[test]
#[ignore = "requires a connected provider and incurs model usage"]
fn compare_planning_prompts() -> Result<(), String> {
    let configured = |name: &str| std::env::var(name).map_err(|_| format!("{name} is required."));
    let model = configured("PRELUDE_EVAL_MODEL")?;
    let selected_case = std::env::var("PRELUDE_EVAL_CASE").ok();
    let cases: Vec<_> = scenarios()
        .into_iter()
        .filter(|case| selected_case.as_ref().is_none_or(|id| id == &case.id))
        .collect();
    if cases.is_empty() {
        return Err("PRELUDE_EVAL_CASE does not match a planning scenario.".into());
    }
    let catalog = load_catalog(vec![])?;
    let catalog_data = read_catalog(&catalog, &json!("list_catalog"), "{}")?;
    let catalog_fingerprint = ring::digest::digest(&ring::digest::SHA256, catalog_data.as_bytes());
    let available_skills = planning_skill_index(&catalog);
    let priorities: std::collections::BTreeMap<_, _> = crate::module_preferences::current()
        .priorities
        .into_iter()
        .filter_map(|(id, rule)| rule.planning.map(|guidance| (id, guidance)))
        .collect();
    let current = json!({"schemaVersion":1, "plan":{
        "harness":{"id":"","version":"","reason":""}, "components":[], "roles":[], "instructions":[]
    }, "assumptions":[]});
    let current_yaml = serde_yaml::to_string(&current).map_err(|error| error.to_string())?;
    tauri::async_runtime::block_on(async {
        let provider = if let Some(storage) = std::env::var_os("PRELUDE_EVAL_CHATGPT_STORAGE") {
            let session = crate::chatgpt::ChatGptSession::default();
            if !crate::chatgpt::restore_session(std::path::Path::new(&storage), &session).await? {
                return Err(
                    "Prelude has no saved ChatGPT session in the evaluation directory.".into(),
                );
            }
            PlainTextProvider::ChatGpt {
                key: crate::chatgpt::authorization(&session)?,
                model: &model,
            }
        } else {
            PlainTextProvider::Api(api_connection(
                &configured("PRELUDE_EVAL_ENDPOINT")?,
                &model,
                &format!("Bearer {}", configured("PRELUDE_EVAL_API_KEY")?),
            )?)
        };
        eprintln!("Model: {model}, catalog: {catalog_fingerprint:?}");
        let mut invalid_candidates = Vec::new();
        for case in cases {
            let input = serde_yaml::to_string(&json!({
                "idea":case.idea, "request":case.idea, "currentPreludeYaml":current_yaml,
                "currentHookChecks":[], "discoveryAnswers":case.discovery_answers, "useDiscoveryDefaults":true,
                "moduleDescriptionLanguage":"English", "technologies":[], "detectedProjectTechnologies":null,
                "projectInstructionSources":null, "availableSkills":available_skills,
                "modulePriorities":priorities, "installedToolIds":catalog.installed_ids, "providers":[],
                "budget":{"amount":0,"currency":"USD","periodDays":30}, "priority":50
            })).map_err(|error| error.to_string())?;
            eprintln!("\n{} rubric: {:?}", case.id, case.criteria);
            for (label, system) in [("baseline", BASELINE), ("candidate", CANDIDATE)] {
                let content = provider
                    .complete(evaluation_messages(system, &input, &catalog_data))
                    .await?;
                let parsed = parse_plan_patch_for_catalog(&content, &catalog).and_then(|patch| {
                    if patch.hook_checks.is_none() {
                        return Err("Initial plan omitted hookChecks.".to_owned());
                    }
                    let plan =
                        serde_json::from_value(json!({"plan":current["plan"], "assumptions":[]}))
                            .map_err(|error| error.to_string())?;
                    let candidate = patch.apply(plan, &catalog)?;
                    let issues = crate::validation::check(&candidate.plan, &catalog.entries);
                    if issues.is_empty() {
                        Ok(())
                    } else {
                        Err(issues.join(" "))
                    }
                });
                if label == "candidate" && parsed.is_err() {
                    invalid_candidates.push(case.id.clone());
                }
                eprintln!("\n{} / {label} / validation {parsed:?}\n{content}", case.id);
            }
        }
        if invalid_candidates.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "Invalid candidate plans: {}",
                invalid_candidates.join(", ")
            ))
        }
    })
}
