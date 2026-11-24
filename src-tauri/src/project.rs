use crate::domain::{validate_profile, ProjectProfile};
use serde::Serialize;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

const MANIFESTS: &[(&str, &str)] = &[
    ("Cargo.toml", "Rust"),
    ("package.json", "JavaScript / TypeScript"),
    ("package-lock.json", "npm lockfile"),
    ("pnpm-lock.yaml", "pnpm lockfile"),
    ("yarn.lock", "Yarn lockfile"),
    ("bun.lock", "Bun lockfile"),
    ("bun.lockb", "Bun lockfile"),
    ("deno.json", "Deno configuration"),
    ("deno.jsonc", "Deno configuration"),
    ("pyproject.toml", "Python"),
    ("go.mod", "Go"),
    ("pom.xml", "Java"),
    ("Gemfile", "Ruby"),
    ("composer.json", "PHP"),
    ("CMakeLists.txt", "C / C++"),
];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectContext {
    pub(crate) profile: ProjectProfile,
    manifests: Vec<&'static str>,
    pub(crate) technologies: Vec<&'static str>,
}

fn detect(root: &Path) -> io::Result<(Vec<&'static str>, Vec<&'static str>)> {
    if !fs::metadata(root)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Expected a directory",
        ));
    }
    let mut manifests = Vec::new();
    let mut technologies = Vec::new();
    for &(name, technology) in MANIFESTS {
        match fs::symlink_metadata(root.join(name)) {
            Ok(metadata) if metadata.is_file() => {
                manifests.push(name);
                technologies.push(technology);
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok((manifests, technologies))
}

#[tauri::command]
pub fn inspect_project(profile: ProjectProfile) -> Result<ProjectContext, String> {
    let profile = validate_profile(profile)?;
    let (manifests, technologies) = profile
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| detect(Path::new(path)).map_err(|error| format!("{path}: {error}")))
        .transpose()?
        .unwrap_or_default();
    Ok(ProjectContext {
        profile,
        manifests,
        technologies,
    })
}

#[derive(Serialize)]
pub struct OpenedProject {
    workspace: Option<crate::workspace::ProjectYaml>,
    plan: Option<crate::domain::Plan>,
    notes: Vec<String>,
    #[serde(rename = "localSkills")]
    local_skills: Vec<String>,
    sources: Vec<ProjectSource>,
    technologies: Vec<&'static str>,
    #[serde(rename = "detectedIds")]
    detected_ids: Vec<String>,
    dependencies: Vec<DeclaredDependency>,
}

#[derive(Serialize, serde::Deserialize)]
pub(crate) struct DeclaredDependency {
    pub ecosystem: String,
    pub package: String,
    pub manifest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    development: bool,
}

pub(crate) fn match_declared_dependencies(
    plan: &mut crate::domain::Plan,
    declared: &[DeclaredDependency],
) {
    for dependency in &mut plan.dependencies {
        dependency.manifest = declared
            .iter()
            .find(|item| {
                item.ecosystem == dependency.ecosystem && item.package == dependency.package
            })
            .map(|item| item.manifest.clone());
    }
}

fn declared_dependencies(
    manifest: &serde_json::Value,
    filename: &str,
    ecosystem: &str,
    production: &str,
    development: &str,
) -> Vec<DeclaredDependency> {
    let mut seen = std::collections::HashSet::new();
    [(production, false), (development, true)]
        .into_iter()
        .filter_map(|(field, development)| {
            manifest[field]
                .as_object()
                .map(|packages| (packages, development))
        })
        .flat_map(|(packages, development)| {
            packages.iter().filter_map(move |(package, value)| {
                if !crate::validation::valid_dependency_name(package) {
                    return None;
                }
                let value = value.as_str()?;
                let version = (value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b".*^~<>=|, +!-_\t".contains(&byte)
                    }))
                .then(|| value.to_owned());
                Some(DeclaredDependency {
                    ecosystem: ecosystem.into(),
                    package: package.clone(),
                    manifest: filename.into(),
                    version,
                    development,
                })
            })
        })
        .filter(|dependency| seen.insert(dependency.package.clone()))
        .take(128)
        .collect()
}

#[derive(Serialize, serde::Deserialize)]
pub struct ProjectSource {
    pub(crate) path: String,
    pub(crate) scope: String,
    pub(crate) content: String,
}

#[tauri::command]
pub fn open_project_directory(
    repository: PathBuf,
    extension_paths: Vec<PathBuf>,
) -> Result<OpenedProject, String> {
    use crate::{
        catalog::load_catalog,
        domain::{Plan, Role, Selection},
        planner::supports,
        preview::{existing_project_directory, is_link, read_existing},
        workspace::{decode_project_yaml, PROJECT_FILE},
    };
    let root = existing_project_directory(&repository)?;
    let workspace = read_existing(&root, PROJECT_FILE)
        .ok()
        .flatten()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|yaml| decode_project_yaml(yaml).ok());
    let catalog = load_catalog(extension_paths)?;
    let safe = |path: &Path| {
        fs::symlink_metadata(path)
            .ok()
            .filter(|metadata| !is_link(metadata))
    };
    let file = |name: &str| safe(&root.join(name)).is_some_and(|metadata| metadata.is_file());
    let agent_dirs = [
        (".codex", "codex"),
        (".claude", "claude-code"),
        (".opencode", "opencode"),
        (".agents", ""),
    ];
    let selection = |id: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| Selection {
                id: entry.id.clone(),
                version: entry.version.clone(),
                reason: "Detected in the project directory.".into(),
            })
    };
    let harnesses: Vec<_> = agent_dirs
        .iter()
        .filter(|(dir, id)| !id.is_empty() && safe(&root.join(dir)).is_some_and(|m| m.is_dir()))
        .map(|(_, id)| *id)
        .collect();
    let harness = match harnesses.as_slice() {
        [only] => selection(only),
        [] if file("CLAUDE.md") => selection("claude-code"),
        [] if file("opencode.json") || file("opencode.jsonc") => selection("opencode"),
        _ => None,
    };
    let mut notes = Vec::new();
    let mut sources = Vec::new();
    let mut remaining = 64 * 1024;
    let mut add_source = |path: &str, scope: &str| {
        if remaining == 0 {
            return;
        }
        let Some(content) = read_existing(&root, path)
            .ok()
            .flatten()
            .filter(|bytes| bytes.len() <= remaining)
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            return;
        };
        remaining -= content.len();
        let mut private_key = false;
        let content = content
            .lines()
            .filter(|line| {
                let upper = line.to_ascii_uppercase();
                if upper.contains("-----BEGIN ") && upper.contains("PRIVATE KEY") {
                    private_key = true;
                }
                if private_key {
                    if upper.contains("-----END ") && upper.contains("PRIVATE KEY") {
                        private_key = false;
                    }
                    return false;
                }
                !contains_credential(line)
            })
            .collect::<Vec<_>>()
            .join("\n");
        sources.push(ProjectSource {
            path: path.into(),
            scope: scope.into(),
            content,
        });
    };
    for path in [
        "AGENTS.md",
        "CLAUDE.md",
        "SKILLS.md",
        ".github/copilot-instructions.md",
    ] {
        add_source(path, ".");
    }
    for parent in [".cursor/rules", ".github/instructions"] {
        let path = root.join(parent);
        if !safe(&path).is_some_and(|metadata| metadata.is_dir()) {
            continue;
        }
        if let Ok(children) = fs::read_dir(path) {
            for child in children.take(64).flatten() {
                let name = child.file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") || name.ends_with(".mdc") {
                    add_source(
                        &format!("{parent}/{name}"),
                        "Apply only according to the rule's frontmatter and file patterns.",
                    );
                }
            }
        }
    }
    for directory in nested_rule_directories(&root) {
        let relative = directory.to_string_lossy().replace('\\', "/");
        let scope = format!("Only for files under {relative}/.");
        for name in ["AGENTS.md", "CLAUDE.md"] {
            add_source(&format!("{relative}/{name}"), &scope);
        }
    }
    let (_, mut technologies) = detect(&root).map_err(|e| e.to_string())?;
    let mut dependencies = Vec::new();
    if let Some(manifest) = read_existing(&root, "package.json")
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        dependencies.extend(declared_dependencies(
            &manifest,
            "package.json",
            "npm",
            "dependencies",
            "devDependencies",
        ));
        for (name, signal) in [
            ("test", "package.json script: test"),
            ("lint", "package.json script: lint"),
            ("typecheck", "package.json script: typecheck"),
        ] {
            if manifest["scripts"][name]
                .as_str()
                .is_some_and(|script| !script.trim().is_empty())
            {
                technologies.push(signal);
            }
        }
    }
    if let Some(manifest) = read_existing(&root, "composer.json")
        .ok()
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    {
        dependencies.extend(
            declared_dependencies(
                &manifest,
                "composer.json",
                "composer",
                "require",
                "require-dev",
            )
            .into_iter()
            .filter(|dependency| dependency.package.contains('/')),
        );
    }
    if !technologies.is_empty() {
        notes.push(format!("Detected stack: {}.", technologies.join(", ")));
    }
    if harnesses.len() > 1 {
        notes.push(format!(
            "Several harnesses detected: {}. Choose one for this environment.",
            harnesses.join(", ")
        ));
    }
    let mut ids = Vec::new();
    if safe(&root.join(".git")).is_some() {
        ids.push("git".into());
    }
    for (manifest, id) in [
        ("Cargo.toml", "rust"),
        ("pyproject.toml", "python"),
        ("requirements.txt", "python"),
        ("go.mod", "go"),
        ("pom.xml", "java"),
        ("build.gradle", "java"),
        ("Gemfile", "ruby"),
        ("composer.json", "php"),
        ("Dockerfile", "docker"),
        ("nginx.conf", "nginx"),
        ("package-lock.json", "npm"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lock", "bun"),
        ("deno.json", "deno"),
    ] {
        if file(manifest) {
            ids.push(id.to_string());
        }
    }
    if ids
        .iter()
        .any(|id| matches!(id.as_str(), "npm" | "pnpm" | "yarn"))
    {
        ids.push("nodejs".into());
    }
    if file("tsconfig.json") {
        ids.push("typescript".into());
    } else if file("package.json") {
        ids.push("javascript".into());
    }
    if [
        "compose.yaml",
        "compose.yml",
        "docker-compose.yml",
        "docker-compose.yaml",
    ]
    .iter()
    .any(|name| file(name))
    {
        ids.extend(["docker".into(), "docker-compose".into()]);
    }
    let mut custom = std::collections::BTreeSet::new();
    for (agent_dir, _) in agent_dirs {
        let parent = root.join(agent_dir);
        if !safe(&parent).is_some_and(|metadata| metadata.is_dir()) {
            continue;
        }
        let skills = parent.join("skills");
        if !safe(&skills).is_some_and(|metadata| metadata.is_dir()) {
            continue;
        }
        if let Ok(children) = fs::read_dir(&skills) {
            for child in children.take(256).flatten() {
                let path = child.path();
                if !safe(&path).is_some_and(|metadata| metadata.is_dir())
                    || !safe(&path.join("SKILL.md")).is_some_and(|metadata| metadata.is_file())
                {
                    continue;
                }
                let name = child.file_name().to_string_lossy().into_owned();
                add_source(
                    &format!("{agent_dir}/skills/{name}/SKILL.md"),
                    "Only when using this local skill.",
                );
                if catalog.entries.iter().any(|entry| {
                    entry.id == name && entry.kind == crate::domain::ComponentKind::Skill
                }) {
                    ids.push(name);
                } else {
                    custom.insert(name);
                }
            }
        }
    }
    let local_skills: Vec<_> = custom.into_iter().collect();
    if !local_skills.is_empty() {
        notes.push("Existing local skills appear in Skills and plugins.".into());
    }
    let mut seen = std::collections::HashSet::new();
    let mut components = Vec::new();
    let selected_harness = harness
        .as_ref()
        .and_then(|item| catalog.entries.iter().find(|entry| entry.id == item.id));
    for id in ids.into_iter().filter(|id| seen.insert(id.clone())) {
        let Some(entry) = catalog.entries.iter().find(|entry| entry.id == id) else {
            continue;
        };
        if selected_harness.is_some_and(|selected| !supports(selected, entry)) {
            notes.push(format!(
                "{} was detected and requires a different harness.",
                entry.name
            ));
        } else if let Some(item) = selection(&id) {
            components.push(item);
        }
    }
    if harness.is_none() {
        notes
            .push("Select a harness or describe the project in chat to complete the setup.".into());
    }
    let harness = harness.or_else(|| {
        workspace
            .as_ref()
            .and_then(|saved| selection(&saved.plan.harness.id))
    });
    let roles = if harness.is_some() {
        vec![Role {
            model_id: None,
            parallel_with: Vec::new(),
            name: "Developer".into(),
            responsibility: "Develop and verify changes in the existing project.".into(),
            interaction: "Report results and handoffs.".into(),
            reason: "Starting role for the existing project.".into(),
        }]
    } else {
        Vec::new()
    };
    let primary = if harness
        .as_ref()
        .is_some_and(|item| item.id == "claude-code")
    {
        "CLAUDE.md"
    } else {
        "AGENTS.md"
    };
    let mut plan = Plan {
        harness: harness.unwrap_or_else(|| Selection {
            id: String::new(),
            version: String::new(),
            reason: String::new(),
        }),
        components,
        dependencies: Vec::new(),
        roles,
        instructions: sources
            .iter()
            .filter(|source| source.path == primary && !source.content.trim().is_empty())
            .flat_map(|source| instruction_sections(&source.content))
            .collect(),
    };
    if let Some(saved) = &workspace {
        plan.dependencies = saved.plan.dependencies.clone();
        match_declared_dependencies(&mut plan, &dependencies);
        for item in &saved.plan.components {
            if !plan.components.iter().any(|found| found.id == item.id) {
                if let Some(restored) = selection(&item.id) {
                    plan.components.push(restored);
                }
            }
        }
        if plan.instructions.is_empty() {
            for instruction in &saved.plan.instructions {
                if !instruction.lines().any(contains_credential) {
                    plan.instructions.push(instruction.clone());
                }
            }
        }
    }
    let detected_ids = std::iter::once(&plan.harness)
        .chain(&plan.components)
        .filter(|item| !item.id.is_empty())
        .map(|item| item.id.clone())
        .collect();
    Ok(OpenedProject {
        workspace,
        plan: Some(plan),
        notes,
        local_skills,
        sources,
        technologies,
        detected_ids,
        dependencies,
    })
}

fn instruction_sections(markdown: &str) -> Vec<String> {
    let mut sections = Vec::new();
    let mut section = String::new();
    let mut fence = None;
    for line in markdown.lines() {
        let marker = ["```", "~~~"]
            .into_iter()
            .find(|marker| line.trim_start().starts_with(marker));
        if let Some(marker) = marker {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
        }
        if fence.is_none() && line.starts_with("## ") && !section.trim().is_empty() {
            sections.push(section.trim_end().to_owned());
            section.clear();
        }
        section.push_str(line);
        section.push('\n');
    }
    if !section.trim().is_empty() {
        sections.push(section.trim_end().into());
    }
    sections
}

fn nested_rule_directories(root: &Path) -> Vec<PathBuf> {
    let mut queued = std::collections::VecDeque::from([(PathBuf::new(), 0)]);
    let mut directories = Vec::new();
    while let Some((relative, depth)) = queued.pop_front() {
        if !relative.as_os_str().is_empty() {
            directories.push(relative.clone());
        }
        if depth >= 3 {
            continue;
        }
        let Ok(children) = fs::read_dir(root.join(&relative)) else {
            continue;
        };
        for entry in children.take(256).flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.')
                || ["node_modules", "target", "dist", "build", "vendor"]
                    .iter()
                    .any(|excluded| name == *excluded)
            {
                continue;
            }
            if directories.len() + queued.len() >= 128 {
                break;
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir())
                && fs::symlink_metadata(entry.path())
                    .is_ok_and(|metadata| !crate::preview::is_link(&metadata))
            {
                queued.push_back((relative.join(name), depth + 1));
            }
        }
    }
    directories
}

fn contains_credential(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("sk-")
        && lower.split_whitespace().any(|word| {
            word.find("sk-")
                .is_some_and(|start| word.len() - start > 24)
        })
        || lower.contains("bearer ")
            && lower.split("bearer ").nth(1).is_some_and(|token| {
                token
                    .split_whitespace()
                    .next()
                    .is_some_and(|value| value.len() > 24)
            })
        || lower.split_once(['=', ':']).is_some_and(|(key, value)| {
            let key = key
                .trim()
                .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            (key.ends_with("_token")
                || key.ends_with("_secret")
                || key.ends_with("_password")
                || key.ends_with("_api_key")
                || matches!(key, "api_key" | "token" | "password"))
                && !value.trim().is_empty()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_direct_application_dependencies_without_exposing_dependency_urls() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        fs::write(root.join("composer.json"), r#"{"require":{"php":"^8.3","easycorp/easyadmin-bundle":"^4"},"require-dev":{"phpunit/phpunit":"^11"}}"#).unwrap();
        fs::write(root.join("package.json"), r#"{"dependencies":{"vue":"^3","private-module":"https://example.com/archive?secret=example"},"devDependencies":{"vitest":"^2"}}"#).unwrap();
        let project = open_project_directory(root.clone(), vec![]).unwrap();
        let names: Vec<_> = project
            .dependencies
            .iter()
            .map(|item| item.package.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "private-module",
                "vue",
                "vitest",
                "easycorp/easyadmin-bundle",
                "phpunit/phpunit"
            ]
        );
        assert!(project.dependencies[0].version.is_none());
        let mut plan = project.plan.unwrap();
        plan.dependencies = serde_json::from_value(serde_json::json!([
            {"ecosystem":"composer","package":"easycorp/easyadmin-bundle","reason":"Build admin lists"},
            {"ecosystem":"npm","package":"unlisted","reason":"Render charts","manifest":"package.json"}
        ])).unwrap();
        match_declared_dependencies(&mut plan, &project.dependencies);
        assert_eq!(
            plan.dependencies[0].manifest.as_deref(),
            Some("composer.json")
        );
        assert!(plan.dependencies[1].manifest.is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepares_analyzed_project_instructions_without_writing_into_the_repo() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join(".codex")).unwrap();
        fs::create_dir_all(root.join(".cursor/rules")).unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::create_dir_all(root.join("packages/web")).unwrap();
        let original = "# Repository rules\n\n## Testing\n\nRun npm test.\n\n## Style\n\nPreserve public APIs.\n";
        fs::write(root.join("AGENTS.md"), original).unwrap();
        fs::write(root.join("SKILLS.md"), "Use Humanizer for public copy.").unwrap();
        fs::write(
            root.join(".github/copilot-instructions.md"),
            "Respect repository conventions.",
        )
        .unwrap();
        fs::write(
            root.join(".cursor/rules/web.mdc"),
            "---\nglobs: packages/web/**\n---\nUse mobile viewports.",
        )
        .unwrap();
        fs::write(
            root.join("packages/web/AGENTS.md"),
            "Only local web rules.\nAPI_KEY=example-secret",
        )
        .unwrap();
        fs::write(root.join(".env"), "IGNORE_ME=secret").unwrap();
        let opened = open_project_directory(root.clone(), vec![]).unwrap();
        let scoped = opened
            .sources
            .iter()
            .find(|source| source.path == "packages/web/AGENTS.md")
            .unwrap();
        assert_eq!(scoped.scope, "Only for files under packages/web/.");
        assert!(!scoped.content.contains("example-secret"));
        assert!(!opened.sources.iter().any(|source| source.path == ".env"));
        assert!(opened
            .sources
            .iter()
            .any(|source| source.path == ".cursor/rules/web.mdc"
                && source.content.contains("packages/web/**")));
        assert!(opened
            .sources
            .iter()
            .any(|source| source.path == "SKILLS.md"));
        assert!(opened
            .sources
            .iter()
            .any(|source| source.path == ".github/copilot-instructions.md"));
        let mut plan = opened.plan.unwrap();
        plan.instructions
            .push("Add a Playwright mobile viewport check.".into());
        let workspace = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1, "description": "Update the existing site.", "plan": plan,
        }))
        .unwrap();
        let result = serde_json::to_value(
            crate::delivery::prepare_agent_instructions(workspace, vec![]).unwrap(),
        )
        .unwrap();
        assert_eq!(result["path"], "AGENTS.md");
        let text = result["content"].as_str().unwrap();
        assert!(
            text.contains("Run npm test.")
                && text.contains("Preserve public APIs.")
                && text.contains("Playwright mobile viewport")
        );
        assert!(!text.contains("Only local web rules."));
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            original
        );
        assert!(!root.join("prelude.yaml").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn opens_existing_setup_or_uses_local_signals() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(root.join(".codex/skills/humanizer")).unwrap();
        fs::create_dir_all(root.join(".agents/skills/custom-skill")).unwrap();
        for path in [
            "Cargo.toml",
            "compose.yaml",
            ".codex/skills/humanizer/SKILL.md",
            ".agents/skills/custom-skill/SKILL.md",
        ] {
            fs::write(root.join(path), "fixture").unwrap();
        }
        let opened = open_project_directory(root.clone(), vec![]).unwrap();
        assert!(opened.workspace.is_none());
        assert_eq!(opened.local_skills, ["custom-skill"]);
        let plan = opened.plan.unwrap();
        assert_eq!(plan.harness.id, "codex");
        assert_eq!(
            plan.components
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["rust", "docker", "docker-compose", "humanizer"]
        );
        let snapshot = serde_json::json!({ "schemaVersion": 1, "description": "Existing fixture", "plan": plan });
        let workspace = serde_json::from_value(snapshot.clone()).unwrap();
        let preview = crate::preview::preview_configuration(
            plan,
            vec![],
            Some(root.clone()),
            Default::default(),
            Some(workspace),
        )
        .unwrap();
        let serialized = serde_json::to_value(preview).unwrap();
        assert!(serialized["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == "prelude.yaml"));
        crate::preview::apply_configuration(
            serde_json::from_value(snapshot["plan"].clone()).unwrap(),
            vec![],
            root.clone(),
            Default::default(),
            serialized["fingerprint"].as_str().unwrap().to_owned(),
            Some(serde_json::from_value(snapshot).unwrap()),
        )
        .unwrap();
        let reopened = open_project_directory(root.clone(), vec![]).unwrap();
        assert!(reopened.workspace.is_some() && reopened.plan.is_some());
        assert!(reopened
            .sources
            .iter()
            .any(|source| source.path == "AGENTS.md"));
        assert!(reopened.detected_ids.iter().any(|id| id == "humanizer"));
        fs::rename(root.join("prelude.yaml"), root.join("mmh.yaml")).unwrap();
        assert!(open_project_directory(root.clone(), vec![])
            .unwrap()
            .plan
            .is_some());
        fs::write(root.join("prelude.yaml"), "invalid YAML").unwrap();
        assert!(open_project_directory(root.clone(), vec![])
            .unwrap()
            .workspace
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_manifests_without_reading_secrets() {
        let root =
            std::env::temp_dir().join(format!("makemeharness-project-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("package.json"), "not parsed or executed").unwrap();
        fs::write(root.join(".env"), "TEST_SECRET=unread").unwrap();
        let result = detect(&root);
        fs::remove_file(root.join("package.json")).unwrap();
        fs::remove_file(root.join(".env")).unwrap();
        fs::remove_dir(&root).unwrap();
        assert_eq!(
            result.unwrap(),
            (vec!["package.json"], vec!["JavaScript / TypeScript"])
        );
    }
}
