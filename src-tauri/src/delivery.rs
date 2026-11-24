use crate::{
    catalog::load_catalog,
    domain::{ComponentDelivery, ComponentKind, Plan, RegistryEntry},
    validation::check,
};
use std::{collections::BTreeMap, fmt::Write, path::PathBuf};

type Files = BTreeMap<String, String>;

#[derive(serde::Serialize)]
pub struct InstructionDocument {
    path: &'static str,
    content: String,
}

#[tauri::command]
pub fn prepare_agent_instructions(
    workspace: crate::workspace::ProjectYaml,
    extension_paths: Vec<PathBuf>,
) -> Result<InstructionDocument, String> {
    let (path, content) = agent_instructions(&workspace.plan, extension_paths, &workspace)?;
    Ok(InstructionDocument { path, content })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExport {
    files: Files,
    omitted: Vec<String>,
    skill_errors: Vec<String>,
}

#[tauri::command]
pub async fn prepare_agent_export(
    workspace: crate::workspace::ProjectYaml,
    extension_paths: Vec<PathBuf>,
    target: String,
    project_sources: Option<Vec<crate::project::ProjectSource>>,
) -> Result<AgentExport, String> {
    let local_skills = workspace.local_skills.clone();
    let (mut export, skills) = prepare_export(workspace, extension_paths, target.clone())?;
    let folder = skill_directory(&target);
    let instruction = if target == "claude-code" {
        "CLAUDE.md"
    } else {
        "AGENTS.md"
    };
    let mut local = BTreeMap::new();
    for source in project_sources.unwrap_or_default() {
        let parts: Vec<_> = source.path.split('/').collect();
        if let [".agents" | ".codex" | ".claude" | ".opencode", "skills", name, "SKILL.md"] =
            parts.as_slice()
        {
            if crate::skill_catalog::valid_export_path(name) {
                local.entry((*name).to_owned()).or_insert(source.content);
            }
        }
    }
    for name in &local_skills {
        if let Some(content) = local.get(name) {
            export
                .files
                .insert(format!("{folder}/{name}/SKILL.md"), content.clone());
        } else {
            export.skill_errors.push(format!(
                "{name}: Local SKILL.md is unavailable. Analyze the source project again."
            ));
        }
    }
    let client = crate::network::client()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("Prelude/0.1 skill export")
        .build()
        .map_err(|error| error.to_string())?;
    for entry in skills {
        let original = format!("{folder}/{}/SKILL.md", entry.id);
        if let Some(content) = local.get(&entry.id) {
            export.files.insert(original, content.clone());
        } else if !export.files.contains_key(&original) {
            match crate::skill_catalog::export_skill(&client, &entry).await {
                Ok(files) => {
                    let name = skill_name(&files["SKILL.md"]).unwrap_or(&entry.id);
                    let prefix = format!("{folder}/{name}/");
                    if export.files.contains_key(&format!("{prefix}SKILL.md")) {
                        export.skill_errors.push(format!(
                            "{}: skill directory {name} is already in the export.",
                            entry.name
                        ));
                        continue;
                    }
                    let resolved = format!("{prefix}SKILL.md");
                    let instructions = export
                        .files
                        .get_mut(instruction)
                        .expect("export instructions");
                    *instructions =
                        instructions.replace(&format!("`{original}`"), &format!("`{resolved}`"));
                    export.files.extend(
                        files
                            .into_iter()
                            .map(|(path, content)| (format!("{prefix}{path}"), content)),
                    );
                }
                Err(error) => export.skill_errors.push(format!("{}: {error}", entry.name)),
            }
        }
    }
    Ok(export)
}

fn skill_name(content: &str) -> Option<&str> {
    let frontmatter = content.trim_start_matches('\u{feff}').strip_prefix("---")?;
    let header = frontmatter
        .lines()
        .skip(1)
        .take_while(|line| line.trim() != "---");
    header
        .filter_map(|line| line.strip_prefix("name:"))
        .map(|name| name.trim().trim_matches(['\'', '"']))
        .find(|name| !name.contains('/') && crate::skill_catalog::valid_export_path(name))
}

fn skill_directory(target: &str) -> &'static str {
    match target {
        "codex" => ".agents/skills",
        "claude-code" => ".claude/skills",
        _ => ".opencode/skills",
    }
}

fn prepare_export(
    mut workspace: crate::workspace::ProjectYaml,
    extension_paths: Vec<PathBuf>,
    target: String,
) -> Result<(AgentExport, Vec<RegistryEntry>), String> {
    if !matches!(target.as_str(), "codex" | "claude-code" | "opencode") {
        return Err("Unsupported export target.".into());
    }
    let catalog = load_catalog(extension_paths.clone())?;
    let harness = catalog
        .entries
        .iter()
        .find(|entry| entry.id == target)
        .ok_or("The export target is missing from the catalog.")?;
    let plan = &mut workspace.plan;
    plan.harness.id = target;
    plan.harness.version = harness.version.clone();
    let mut omitted = Vec::new();
    plan.components.retain(|item| {
        let entry = catalog.entries.iter().find(|entry| entry.id == item.id);
        if let Some(entry) = entry.filter(|entry| !crate::planner::supports(harness, entry)) {
            omitted.push(entry.name.clone());
            false
        } else {
            true
        }
    });
    for role in &mut plan.roles {
        if role
            .model_id
            .as_ref()
            .is_some_and(|id| !plan.components.iter().any(|item| &item.id == id))
        {
            role.model_id = None;
        }
    }
    let (path, content) = agent_instructions(&workspace.plan, extension_paths, &workspace)?;
    let mut files = generate(&workspace.plan)?;
    add_modules(&workspace.plan, &catalog.entries, &mut files)?;
    files.remove("MAKE_ME_HARNESS_SETUP.md");
    files.insert(path.into(), content);
    let skills = catalog
        .entries
        .into_iter()
        .filter(|entry| {
            entry.kind == ComponentKind::Skill
                && workspace
                    .plan
                    .components
                    .iter()
                    .any(|selected| selected.id == entry.id)
        })
        .collect();
    add_planned_hooks(&workspace, &mut files)?;
    Ok((
        AgentExport {
            files,
            omitted,
            skill_errors: Vec::new(),
        },
        skills,
    ))
}

fn add_planned_hooks(
    workspace: &crate::workspace::ProjectYaml,
    files: &mut Files,
) -> Result<(), String> {
    use crate::ai_plan::HookCheck;
    if workspace.plan.harness.id != "codex" || workspace.hook_checks.is_empty() {
        return Ok(());
    }
    let Some(manager) = workspace
        .plan
        .components
        .iter()
        .map(|item| item.id.as_str())
        .find(|id| matches!(*id, "npm" | "pnpm" | "yarn" | "bun"))
    else {
        return Ok(());
    };
    let checks: Vec<_> = [
        ("test", HookCheck::Test),
        ("lint", HookCheck::Lint),
        ("typecheck", HookCheck::Typecheck),
    ]
    .into_iter()
    .filter_map(|(name, check)| workspace.hook_checks.contains(&check).then_some(name))
    .collect();
    let config = serde_json::json!({"hooks": {"Stop": [{"hooks": [{
        "type": "command",
        "command": format!("node .codex/hooks/run-checks.cjs {manager} {}", checks.join(" ")),
        "timeout": 120,
        "statusMessage": "Running planned project checks"
    }]}]}});
    files.insert(
        ".codex/hooks.json".into(),
        serde_json::to_string_pretty(&config).map_err(|error| error.to_string())? + "\n",
    );
    files.insert(
        ".codex/hooks/run-checks.cjs".into(),
        include_str!("run-project-checks.cjs").into(),
    );
    if let Some(instructions) = files.get_mut("AGENTS.md") {
        let mut manual = vec!["local start", "build"];
        if !checks.contains(&"test") {
            manual.push("test");
        }
        if !checks.contains(&"typecheck") {
            manual.push("type check");
        }
        *instructions = instructions.replace(
            "Run applicable checks, such as a local start, type check, test or build.",
            &format!(
                "Run applicable manual checks ({}) when relevant.",
                manual.join(", ")
            ),
        );
        let _ = writeln!(instructions, "\n## Automated checks\n\nThe Codex Stop hook runs the planned scripts ({}) using {manager}. Let the trusted hook run these routine checks. Run them manually when diagnosing failures or when the hook is disabled or skipped.", checks.join(", "));
    }
    Ok(())
}

#[tauri::command]
pub async fn save_agent_export(
    window: tauri::WebviewWindow,
    files: Files,
    dialog_title: String,
) -> Result<bool, String> {
    use std::io::Write;
    use tauri_plugin_dialog::DialogExt;
    let zip = archive_contents(files)?;
    let Some(location) = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title(dialog_title)
        .set_file_name("prelude.zip")
        .add_filter("ZIP", &["zip"])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = location.into_path().map_err(|error| error.to_string())?;
    let directory = path.parent().ok_or("Choose a folder for the archive.")?;
    let temp = directory.join(format!(".prelude-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&zip)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &path)
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("Could not save ZIP: {error}"));
    }
    Ok(true)
}

fn archive_contents(files: Files) -> Result<Vec<u8>, String> {
    use std::io::{Cursor, Write};
    if files.is_empty()
        || files.len() > 512
        || files.values().map(String::len).sum::<usize>() > 16 * 1024 * 1024
    {
        return Err("The export has no files or exceeds the ZIP size limit.".into());
    }
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (path, text) in files {
        if !path.split('/').all(|part| {
            !matches!(part, "" | "." | "..")
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        }) {
            return Err(format!("Invalid archive path: {path}"));
        }
        writer
            .start_file(path, zip::write::SimpleFileOptions::default())
            .map_err(|error| error.to_string())?;
        writer
            .write_all(text.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| error.to_string())
}

pub(crate) fn agent_instructions(
    plan: &Plan,
    extension_paths: Vec<PathBuf>,
    workspace: &crate::workspace::ProjectYaml,
) -> Result<(&'static str, String), String> {
    let catalog = load_catalog(extension_paths)?;
    let mut issues = catalog.errors;
    issues.extend(check(plan, &catalog.entries));
    if !issues.is_empty() {
        return Err(issues.join("\n"));
    }
    let path = rules_path(plan)?;
    let mut text = String::from("# Project instructions\n");
    if !workspace.description.trim().is_empty() {
        let _ = writeln!(
            text,
            "\n## Project brief\n\n{}",
            workspace.description.trim()
        );
    }
    text.push_str("\n## First task: prepare the environment\n\nWhen the user asks to prepare the environment, inspect the existing files, manifests and lockfiles. Preserve working conventions and report conflicts before changing the stack. Prepare missing project files as needed, connect the selected skills and tools using the agent's native configuration, and install project dependencies with the selected package manager. Follow the project-specific commands below when available and derive missing commands from the verified project manifests. Run applicable checks, such as a local start, type check, test or build. Report the actual results and any concrete failures or steps that need the user's participation. Keep credentials outside tracked files.\n");
    text.push_str("\n## Selected environment\n\n");
    let _ = writeln!(text, "- Agent: `{}`", plan.harness.id);
    for selected in &plan.components {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.id == selected.id)
            .ok_or_else(|| format!("Module {} is missing from the catalog.", selected.id))?;
        if entry.kind == ComponentKind::Skill {
            continue;
        }
        let version = if selected.version == "unknown" {
            ""
        } else {
            &selected.version
        };
        let _ = writeln!(
            text,
            "- {} {} — {}",
            entry.name,
            version,
            selected.reason.trim()
        );
        if let Some(link) = entry
            .setup_docs
            .as_deref()
            .or(Some(entry.source.as_str()))
            .and_then(|url| public_url(url).ok())
        {
            let _ = writeln!(text, "  - Setup: <{link}>");
        }
        if let Some(ComponentDelivery::RemoteMcp { url }) = &entry.delivery {
            let url = public_url(url)?;
            let _ = writeln!(text, "  - MCP endpoint: <{url}>");
        }
    }
    if !plan.roles.is_empty() {
        text.push_str("\n## Roles\n");
        for role in &plan.roles {
            let _ = writeln!(
                text,
                "\n### {}\n\n{}\n\n{}",
                role.name, role.responsibility, role.interaction
            );
            if let Some(model) = &role.model_id {
                let _ = writeln!(text, "\nModel: `{model}`.");
            }
            if !role.parallel_with.is_empty() {
                let _ = writeln!(
                    text,
                    "\nCan work in parallel with {}.",
                    role.parallel_with.join(", ")
                );
            }
        }
    }
    if !plan.instructions.is_empty() {
        let _ = writeln!(
            text,
            "\n## Project-specific setup and checks\n\n{}",
            plan.instructions.join("\n\n")
        );
    }
    text.push_str(&project_skill_instructions(
        plan,
        &catalog.entries,
        workspace,
    ));
    Ok((path, text))
}

fn project_skill_instructions(
    plan: &Plan,
    entries: &[RegistryEntry],
    workspace: &crate::workspace::ProjectYaml,
) -> String {
    let mut text = String::new();
    let skills: Vec<_> = plan
        .components
        .iter()
        .filter_map(|selected| {
            entries
                .iter()
                .find(|entry| entry.id == selected.id && entry.kind == ComponentKind::Skill)
                .map(|entry| (selected, entry))
        })
        .collect();
    if !skills.is_empty() || !workspace.local_skills.is_empty() {
        let folder = match plan.harness.id.as_str() {
            "codex" => ".agents/skills",
            "claude-code" => ".claude/skills",
            _ => ".opencode/skills",
        };
        text.push_str("\n## Project skills\n\nRead each relevant skill's SKILL.md before using it. Reuse existing project skills. For a missing external skill, verify the source and use the pinned revision when listed. Connect it in the agent's project skill directory. Report an unavailable source or revision before choosing a substitute.\n");
        for name in &workspace.local_skills {
            let _ = writeln!(text, "\n- Existing local skill: {name}. Locate its SKILL.md in the repository and apply it when its description matches the task.");
        }
        let preferences = crate::module_preferences::current();
        for (selected, entry) in skills {
            let purpose = entry
                .purpose
                .as_deref()
                .unwrap_or("Use for tasks that match its description.");
            let when = preferences
                .priorities
                .get(&entry.id)
                .and_then(|rule| rule.instructions.as_deref())
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    let reason = selected.reason.trim();
                    if reason.is_empty() {
                        purpose
                    } else {
                        reason
                    }
                });
            let _ = writeln!(text, "\n### {}\n\n- Purpose: {}\n- When to use: {}\n- Project location: `{}/{}/SKILL.md`", entry.name, purpose, when, folder, entry.id);
            if !matches!(entry.delivery, Some(ComponentDelivery::Skill { .. })) {
                text.push_str(
                    "- External skill. Reuse an installed copy or fetch it from the source.\n",
                );
            }
            if let Ok(source) = public_url(&entry.source) {
                let _ = writeln!(text, "- Source: <{source}>");
            } else {
                text.push_str(
                    "- Source unavailable. Ask for the approved source before installing.\n",
                );
            }
            if selected.version != "unknown" && !selected.version.is_empty() {
                let _ = writeln!(
                    text,
                    "- Catalog version or revision: `{}`",
                    selected.version
                );
            }
            if let Some(ComponentDelivery::Skill {
                description,
                instructions,
            }) = &entry.delivery
            {
                let _ = writeln!(
                    text,
                    "- Local skill description: {description}\n\n{instructions}"
                );
            }
        }
    }
    text
}

#[tauri::command]
pub fn generate_configuration(plan: Plan, extension_paths: Vec<PathBuf>) -> Result<Files, String> {
    generate_build_files(plan, extension_paths).map(|build| build.files)
}

pub(crate) struct BuildFiles {
    pub(crate) files: Files,
    pub(crate) setup_required: Vec<String>,
}

pub(crate) fn generate_build_files(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
) -> Result<BuildFiles, String> {
    let catalog = load_catalog(extension_paths)?;
    let mut issues = catalog.errors;
    issues.extend(check(&plan, &catalog.entries));
    if !issues.is_empty() {
        return Err(issues.join("\n"));
    }
    let mut files = generate(&plan)?;
    let setup_required = add_modules(&plan, &catalog.entries, &mut files)?;
    Ok(BuildFiles {
        files,
        setup_required,
    })
}

fn public_url(value: &str) -> Result<String, String> {
    let url =
        reqwest::Url::parse(value).map_err(|_| "The module URL must be a public HTTPS URL.")?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "The module URL must use HTTPS and contain no credentials or query parameters.".into(),
        );
    }
    Ok(url.into())
}

fn add_modules(
    plan: &Plan,
    entries: &[RegistryEntry],
    files: &mut Files,
) -> Result<Vec<String>, String> {
    use serde_json::{json, Map};
    let mut mcp = Map::new();
    let mut codex_config = String::new();
    let mut guide = String::new();
    let mut skill_rules = String::new();
    let mut setup_required = Vec::new();
    let kind = plan.harness.id.as_str();
    for selected in &plan.components {
        let entry = entries
            .iter()
            .find(|entry| entry.id == selected.id)
            .ok_or_else(|| format!("Module {} is missing from the catalog.", selected.id))?;
        match &entry.delivery {
            Some(ComponentDelivery::Skill {
                description,
                instructions,
            }) if entry.kind == ComponentKind::Skill => {
                let folder = match kind {
                    "codex" => ".agents/skills",
                    "claude-code" => ".claude/skills",
                    _ => ".opencode/skills",
                };
                let description = serde_json::to_string(description).map_err(|e| e.to_string())?;
                files.insert(
                    format!("{folder}/{}/SKILL.md", entry.id),
                    format!(
                        "---\nname: {}\ndescription: {description}\n---\n\n{}\n",
                        entry.id,
                        instructions.trim()
                    ),
                );
            }
            Some(ComponentDelivery::RemoteMcp { url }) if entry.kind == ComponentKind::Mcp => {
                let url = public_url(url)?;
                match kind {
                    "codex" => codex_config.push_str(&format!(
                        "[mcp_servers.{}]\nurl = {}\n\n",
                        toml_string(&entry.id),
                        toml_string(&url)
                    )),
                    "claude-code" => {
                        mcp.insert(entry.id.clone(), json!({"type": "http", "url": url}));
                    }
                    _ => {
                        mcp.insert(entry.id.clone(), json!({"type": "remote", "url": url}));
                    }
                }
            }
            Some(_) => {
                return Err(format!(
                    "The setup method does not match the type of {}.",
                    entry.id
                ))
            }
            None => {
                setup_required.push(entry.name.clone());
                let link = public_url(entry.setup_docs.as_deref().unwrap_or(&entry.source))?;
                if entry.kind == ComponentKind::Skill {
                    use std::fmt::Write;
                    let purpose = entry
                        .purpose
                        .as_deref()
                        .unwrap_or("Follow its upstream instructions when relevant to the task.");
                    let _ = writeln!(
                        skill_rules,
                        "- **{}** — {}. Source: <{}>",
                        entry.name,
                        purpose.trim_end_matches('.'),
                        link
                    );
                }
                guide.push_str(&format!(
                    "- `{}` · version `{}` · <{}>\n",
                    entry.id, entry.version, link
                ));
            }
        }
    }
    if !skill_rules.is_empty() {
        let rules = files
            .get_mut(rules_path(plan)?)
            .expect("generated project instructions");
        rules.push_str("\n## Project skills\n\nUse the selected skills for the tasks listed below. If a skill is already installed, use its local copy. Otherwise read its upstream instructions from the source before applying it. If the source is unavailable, report that and continue with the project instructions.\n\n");
        rules.push_str(&skill_rules);
    }
    if !guide.is_empty() {
        files.insert(
            "MAKE_ME_HARNESS_SETUP.md".into(),
            format!("# Module setup\n\n{guide}"),
        );
    }
    if !codex_config.is_empty() {
        files.insert(".codex/config.toml".into(), codex_config);
    }
    if !mcp.is_empty() {
        let (path, config) = if kind == "claude-code" {
            (".mcp.json", json!({"mcpServers": mcp}))
        } else {
            (
                "opencode.json",
                json!({"$schema": "https://opencode.ai/config.json", "mcp": {"servers": mcp}}),
            )
        };
        files.insert(
            path.into(),
            serde_json::to_string_pretty(&config).map_err(|e| e.to_string())? + "\n",
        );
    }
    Ok(setup_required)
}

pub(crate) fn rules_path(plan: &Plan) -> Result<&'static str, String> {
    Ok(match plan.harness.id.as_str() {
        "codex" | "opencode" => "AGENTS.md",
        "claude-code" => "CLAUDE.md",
        _ => {
            return Err(
                "The selected harness does not have a project generation adapter yet.".into(),
            )
        }
    })
}

fn generate(plan: &Plan) -> Result<Files, String> {
    let rules_path = rules_path(plan)?;
    let mut rules = plan.instructions.join("\n\n");
    if let Some(role) = plan.roles.first() {
        rules.push_str(&format!(
            "\n\n## Primary role\n\n{}\n\n{}",
            role.responsibility.trim(),
            role.interaction.trim()
        ));
    }
    rules.push('\n');
    let mut files = BTreeMap::from([(rules_path.into(), rules)]);
    for (index, role) in plan.roles.iter().enumerate().skip(1) {
        let name = format!("role-{}", index + 1);
        let description = role.name.trim();
        let instructions = format!(
            "{}\n\n{}\n",
            role.responsibility.trim(),
            role.interaction.trim()
        );
        let (path, body) = match plan.harness.id.as_str() {
            "codex" => (
                format!(".codex/agents/{name}.toml"),
                format!(
                    "name = {name:?}\ndescription = {}\ndeveloper_instructions = {}\n",
                    toml_string(description),
                    toml_string(&instructions)
                ),
            ),
            "claude-code" => (
                format!(".claude/agents/{name}.md"),
                format!(
                    "---\nname: {name}\ndescription: {}\n---\n\n{instructions}",
                    serde_json::to_string(description).map_err(|e| e.to_string())?
                ),
            ),
            "opencode" => (
                format!(".opencode/agents/{name}.md"),
                format!(
                    "---\ndescription: {}\nmode: subagent\n---\n\n{instructions}",
                    serde_json::to_string(description).map_err(|e| e.to_string())?
                ),
            ),
            _ => unreachable!("checked harness above"),
        };
        files.insert(path, body);
    }
    Ok(files)
}

fn toml_string(text: &str) -> String {
    use std::fmt::Write;
    let mut output = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(&mut output, "\\u{:04X}", u32::from(c));
            }
            c => output.push(c),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn export_uses_target_agent_paths_and_archives_edited_text() {
        use std::io::{Cursor, Read};
        let workspace = serde_json::from_value(json!({
            "schemaVersion": 1,
            "description": "Fixture project",
            "localSkills": ["internal-review"],
            "plan": {
                "harness": {"id":"codex","version":"0.167.1","reason":"Selected"},
                "components": [],
                "roles": [
                    {"name":"Developer","responsibility":"Implement","interaction":"Report","reason":"Primary"},
                    {"name":"Reviewer","responsibility":"Review","interaction":"Report","reason":"Secondary"}
                ],
                "instructions": ["Run tests."]
            }
        })).unwrap();
        let skill =
            "---\nname: internal-review\ndescription: Review changes\n---\n\nCheck the diff.\n";
        let mut export = tauri::async_runtime::block_on(prepare_agent_export(
            workspace,
            vec![],
            "claude-code".into(),
            Some(vec![crate::project::ProjectSource {
                path: ".agents/skills/internal-review/SKILL.md".into(),
                scope: String::new(),
                content: skill.into(),
            }]),
        ))
        .unwrap();
        assert_eq!(
            export.files[".claude/skills/internal-review/SKILL.md"],
            skill
        );
        assert!(!export.files.contains_key("SKILLS.md"));
        assert!(export.files.contains_key(".claude/agents/role-2.md"));
        assert!(!export.files.contains_key("AGENTS.md"));
        export
            .files
            .insert("CLAUDE.md".into(), "Edited by the user.".into());
        let data = archive_contents(export.files).unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(data)).unwrap();
        let mut text = String::new();
        zip.by_name("CLAUDE.md")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "Edited by the user.");
        assert!(
            archive_contents(BTreeMap::from([("../outside.md".into(), String::new())])).is_err()
        );
    }

    #[test]
    fn exports_planned_checks_for_codex_and_leaves_other_targets_unchanged() {
        let workspace = || {
            serde_json::from_value(json!({
            "schemaVersion": 1,
            "description": "Web application",
            "hookChecks": ["lint", "test"],
            "plan": {
                "harness": {"id":"codex","version":"0.167.1","reason":"Selected"},
                "components": [
                    {"id":"nodejs","version":"unknown","reason":"Runtime"},
                    {"id":"npm","version":"unknown","reason":"Scripts"}
                ],
                "roles": [{"name":"Developer","responsibility":"Implement","interaction":"Report","reason":"Primary"}],
                "instructions": []
            }
        })).unwrap()
        };
        let (codex, _) = prepare_export(workspace(), vec![], "codex".into()).unwrap();
        let config: serde_json::Value =
            serde_json::from_str(&codex.files[".codex/hooks.json"]).unwrap();
        assert_eq!(
            config["hooks"]["Stop"][0]["hooks"][0]["command"],
            "node .codex/hooks/run-checks.cjs npm test lint"
        );
        assert!(codex.files["AGENTS.md"]
            .contains("The Codex Stop hook runs the planned scripts (test, lint)"));
        let (claude, _) = prepare_export(workspace(), vec![], "claude-code".into()).unwrap();
        assert!(!claude.files.contains_key(".codex/hooks.json"));
        assert!(claude.files["CLAUDE.md"].contains("type check, test or build"));
    }

    #[test]
    fn generates_module_files_and_rejects_credentials_in_endpoint() {
        let mut catalog = load_catalog(vec![]).unwrap();
        for entry in [
            json!({"schemaVersion":1,"id":"review","name":"Review","kind":"skill","source":"https://example.com/skill","version":"1","requires":[],"conflicts":[],"harnessVersions":{},"delivery":{"type":"skill","description":"Review changes","instructions":"Run tests"}}),
            json!({"schemaVersion":1,"id":"docs","name":"Docs","kind":"mcp","source":"https://example.com","version":"1","requires":[],"conflicts":[],"harnessVersions":{},"delivery":{"type":"remote_mcp","url":"https://example.com/mcp"}}),
        ] {
            catalog.entries.push(serde_json::from_value(entry).unwrap());
        }
        for (harness, version, path, config) in [
            (
                "codex",
                "0.167.1",
                ".agents/skills/review/SKILL.md",
                ".codex/config.toml",
            ),
            (
                "claude-code",
                "2.1.322",
                ".claude/skills/review/SKILL.md",
                ".mcp.json",
            ),
            (
                "opencode",
                "2.2.1",
                ".opencode/skills/review/SKILL.md",
                "opencode.json",
            ),
        ] {
            let plan = serde_json::from_value(json!({
                "harness":{"id":harness,"version":version,"reason":""},
                "components":[{"id":"review","version":"1","reason":""},{"id":"docs","version":"1","reason":""}],
                "roles":[], "instructions":[]
            })).unwrap();
            let mut files = Files::new();
            add_modules(&plan, &catalog.entries, &mut files).unwrap();
            assert!(files[path].contains("Run tests"));
            assert!(files[config].contains("https://example.com/mcp"));
        }
        assert!(public_url("https://example.com/mcp?token=secret").is_err());
        assert!(public_url("https://user:secret@example.com/mcp").is_err());
    }

    #[test]
    fn writes_rules_and_roles_to_harness_specific_paths() {
        for (harness, version, rules, agent) in [
            ("codex", "0.167.1", "AGENTS.md", ".codex/agents/role-2.toml"),
            (
                "claude-code",
                "2.1.322",
                "CLAUDE.md",
                ".claude/agents/role-2.md",
            ),
            (
                "opencode",
                "2.2.1",
                "AGENTS.md",
                ".opencode/agents/role-2.md",
            ),
        ] {
            let plan = serde_json::from_value(json!({
                "harness": {"id": harness, "version": version, "reason": "selected"},
                "components": [],
                "roles": [
                    {"name": "Developer", "responsibility": "Implement", "interaction": "Report", "reason": "required"},
                    {"name": "Reviewer", "responsibility": "Review", "interaction": "Send findings", "reason": "required"}
                ],
                "instructions": ["Run tests"]
            })).unwrap();
            let files = generate_configuration(plan, vec![]).unwrap();
            assert_eq!(files.len(), 2);
            assert!(files[rules].contains("Run tests"));
            assert!(files[agent].contains("Send findings"));
            assert!(!files[agent].contains("required"));
        }
    }
}
