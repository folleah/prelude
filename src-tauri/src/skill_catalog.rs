use crate::domain::{ComponentKind, RegistryEntry};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::async_runtime::Mutex;

const SOURCES: &[&str] = &[
    "anthropics/skills",
    "vercel-labs/agent-skills",
    "obra/superpowers",
    "addyosmani/agent-skills",
    "microsoft/playwright",
];
const REFRESH_INTERVAL: u64 = 24 * 60 * 60;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SKILLS_PER_SOURCE: usize = 40;
const ARCHIVED: &str = "archived-skill";
static CACHE_PATH: OnceLock<PathBuf> = OnceLock::new();
static SYNC_LOCK: Mutex<()> = Mutex::const_new(());

pub(crate) fn initialize(directory: PathBuf) {
    let _ = CACHE_PATH.set(directory.join("skills-catalog-v1.json"));
}

#[tauri::command]
pub(crate) fn skill_catalog_file() -> Option<(PathBuf, bool)> {
    let path = CACHE_PATH.get()?;
    Some((path.clone(), path.is_file()))
}

#[tauri::command]
pub(crate) fn open_skill_catalog_file() -> Result<(), String> {
    let path = CACHE_PATH
        .get()
        .filter(|path| path.is_file())
        .ok_or("The local catalog file is not available yet.")?;
    #[cfg(target_os = "windows")]
    let opener = "notepad.exe";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let opener = "xdg-open";
    std::process::Command::new(opener)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open the local catalog file: {error}"))
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    last_attempt: u64,
    sources: BTreeMap<String, Snapshot>,
    #[serde(default)]
    discovered: BTreeMap<String, (String, Skill)>,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    current: Vec<String>,
    skills: BTreeMap<String, Skill>,
}

#[derive(Serialize, Deserialize)]
struct Skill {
    slug: String,
    path: String,
    commit: String,
    revision: String,
}

#[derive(Deserialize)]
struct Commit {
    sha: String,
}

#[derive(Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    sha: String,
}

fn read_cache(path: &Path) -> Cache {
    fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn write_cache(path: &Path, cache: &Cache) -> Result<(), String> {
    let directory = path
        .parent()
        .ok_or("Could not locate the skills cache directory.")?;
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(cache).map_err(|error| error.to_string())?;
    let temp = directory.join(format!("skills-cache-{}.tmp", uuid::Uuid::new_v4()));
    let result = fs::write(&temp, bytes).and_then(|_| fs::rename(&temp, path));
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(|error| error.to_string())
}

pub(crate) fn cached_entries() -> Vec<RegistryEntry> {
    let Some(path) = CACHE_PATH.get() else {
        return Vec::new();
    };
    let cache = read_cache(path);
    let preferences = crate::module_preferences::current();
    let mut entries: BTreeMap<_, _> = cache
        .discovered
        .iter()
        .map(|(id, (source, skill))| {
            (
                id,
                cache_entry(
                    source,
                    id,
                    skill,
                    !preferences.excludes_skill(source, &skill.slug),
                ),
            )
        })
        .collect();
    entries.extend(
        SOURCES
            .iter()
            .filter_map(|source| {
                cache
                    .sources
                    .get(*source)
                    .map(|snapshot| (*source, snapshot))
            })
            .flat_map(|(source, snapshot)| {
                let current: BTreeSet<_> = snapshot.current.iter().collect();
                let preferences = &preferences;
                snapshot.skills.iter().map(move |(id, skill)| {
                    let active =
                        current.contains(id) && !preferences.excludes_skill(source, &skill.slug);
                    (id, cache_entry(source, id, skill, active))
                })
            }),
    );
    entries.into_values().collect()
}

#[derive(Deserialize)]
struct SearchResults {
    skills: Vec<SearchHit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchHit {
    source: String,
    skill_id: String,
    installs: u64,
}

pub(crate) struct DiscoveredSkill {
    pub entry: RegistryEntry,
    pub installs: u64,
    source: String,
    skill: Skill,
}

impl DiscoveredSkill {
    pub(crate) fn catalog_entry(&self) -> RegistryEntry {
        cache_entry(&self.source, &self.entry.id, &self.skill, true)
    }
}

pub(crate) async fn search_skills(query: &str) -> Result<Vec<DiscoveredSkill>, String> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 100 || query.chars().any(char::is_control) {
        return Err("Use a short skill search query of up to 100 characters.".into());
    }
    let client = crate::network::client()
        .user_agent("Prelude/0.1 skill catalog")
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut url = reqwest::Url::parse("https://skills.sh/api/search").expect("static URL");
    url.query_pairs_mut()
        .append_pair("q", query)
        .append_pair("limit", "8");
    let mut hits = get::<SearchResults>(&client, url.to_string()).await?.skills;
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.installs));
    let jobs: Vec<_> = hits
        .into_iter()
        .filter(|hit| {
            let valid_segment = |segment: &str| {
                !segment.is_empty()
                    && segment.len() <= 100
                    && segment != "."
                    && segment != ".."
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            };
            hit.source.split('/').count() == 2
                && hit.source.split('/').all(valid_segment)
                && valid_segment(&hit.skill_id)
        })
        .take(5)
        .map(|hit| {
            let client = client.clone();
            tauri::async_runtime::spawn(async move {
                let (id, skill) =
                    list_source_with_target(&client, &hit.source, Some(&hit.skill_id))
                        .await
                        .ok()?
                        .into_iter()
                        .next()?;
                Some(DiscoveredSkill {
                    entry: cache_entry(&hit.source, &id, &skill, true),
                    installs: hit.installs,
                    source: hit.source,
                    skill,
                })
            })
        })
        .collect();
    let mut matches = Vec::new();
    for job in jobs {
        if let Ok(Some(skill)) = job.await {
            matches.push(skill);
        }
    }
    Ok(matches)
}

pub(crate) async fn remember_discovered(
    matches: Vec<DiscoveredSkill>,
    plan: &crate::domain::Plan,
) -> Result<(), String> {
    let selected: Vec<_> = matches
        .into_iter()
        .filter(|skill| {
            plan.components
                .iter()
                .any(|component| component.id == skill.entry.id)
        })
        .collect();
    if selected.is_empty() {
        return Ok(());
    }
    let path = CACHE_PATH
        .get()
        .ok_or("Could not locate the local skills catalog.")?;
    let _sync = SYNC_LOCK.lock().await;
    let mut cache = read_cache(path);
    for skill in selected {
        cache
            .discovered
            .insert(skill.entry.id, (skill.source, skill.skill));
    }
    write_cache(path, &cache)
}

fn cache_entry(source: &str, id: &str, skill: &Skill, current: bool) -> RegistryEntry {
    let url = format!(
        "https://github.com/{source}/blob/{}/{}",
        skill.commit, skill.path
    );
    let mut categories = vec!["github-skill".into(), "free-tool".into()];
    if !current {
        categories.push(ARCHIVED.into());
    }
    RegistryEntry {
        schema_version: 1,
        id: id.into(),
        name: skill.slug.replace('-', " "),
        kind: ComponentKind::Skill,
        source: url.clone(),
        version: skill.revision.clone(),
        requires: Vec::new(),
        conflicts: Vec::new(),
        harness_versions: BTreeMap::new(),
        purpose: Some(format!(
            "Agent skill {} from {source}. Review its setup requirements at the source.",
            skill.slug
        )),
        categories,
        setup_docs: Some(url),
        delivery: None,
        model: None,
        project_tags: Vec::new(),
    }
}

pub(crate) fn is_archived(entry: &RegistryEntry) -> bool {
    entry.categories.iter().any(|category| category == ARCHIVED)
}

fn valid_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn skill_entries(
    source: &str,
    commit: String,
    tree: Tree,
    target: Option<&str>,
) -> Result<BTreeMap<String, Skill>, String> {
    if tree.truncated || !valid_sha(&commit) {
        return Err("Incomplete GitHub skill listing.".into());
    }
    let mut result = BTreeMap::new();
    let preferences = crate::module_preferences::current();
    let directories: BTreeMap<_, _> = tree
        .tree
        .iter()
        .filter(|entry| entry.mode == "040000" && valid_sha(&entry.sha))
        .map(|entry| (entry.path.clone(), entry.sha.clone()))
        .collect();
    let prefix = if source == "microsoft/playwright" {
        "packages/playwright-core/src/tools/skills/"
    } else {
        "skills/"
    };
    for entry in tree.tree {
        if !matches!(entry.mode.as_str(), "100644" | "100755") || !valid_sha(&entry.sha) {
            continue;
        }
        if target.is_some()
            && entry.path.split('/').any(|part| {
                part.is_empty()
                    || matches!(part, "." | "..")
                    || !part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
                    })
            })
        {
            continue;
        }
        let Some(slug) = entry
            .path
            .strip_suffix("/SKILL.md")
            .and_then(|path| match target {
                Some(target) => path.rsplit('/').next().filter(|slug| *slug == target),
                None => path.strip_prefix(prefix),
            })
        else {
            continue;
        };
        if slug.is_empty()
            || slug.len() > 80
            || !slug
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            continue;
        }
        if preferences.excludes_skill(source, slug) {
            continue;
        }
        let directory = entry.path.trim_end_matches("/SKILL.md");
        let revision = directories.get(directory).cloned().unwrap_or(entry.sha);
        let identity = format!("{source}/{}@{revision}", entry.path);
        let hash = ring::digest::digest(&ring::digest::SHA256, identity.as_bytes());
        let suffix: String = hash.as_ref()[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let id = format!("github-{slug}-{suffix}");
        result.insert(
            id,
            Skill {
                slug: slug.into(),
                path: entry.path,
                commit: commit.clone(),
                revision,
            },
        );
        if result.len() >= MAX_SKILLS_PER_SOURCE {
            break;
        }
    }
    Ok(result)
}

async fn get<T: serde::de::DeserializeOwned>(client: &Client, url: String) -> Result<T, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if data.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err("GitHub skill listing exceeds the size limit.".into());
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(|error| error.to_string())
}

async fn list_source(client: &Client, source: &str) -> Result<BTreeMap<String, Skill>, String> {
    list_source_with_target(client, source, None).await
}

pub(crate) async fn export_skill(
    client: &Client,
    entry: &RegistryEntry,
) -> Result<BTreeMap<String, String>, String> {
    let url = reqwest::Url::parse(&entry.source).map_err(|_| "Invalid skill source.")?;
    if url.scheme() != "https" || url.host_str() != Some("github.com") {
        return Err("A GitHub source is required to download this skill.".into());
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if !parts.iter().all(|part| valid_export_path(part)) {
        return Err("Invalid GitHub skill path.".into());
    }
    let [owner, repo, tail @ ..] = parts.as_slice() else {
        return Err("Invalid GitHub repository.".into());
    };
    let api = format!("https://api.github.com/repos/{owner}/{repo}");
    let (commit, explicit_directory) = match tail {
        [kind @ ("blob" | "tree"), reference, path @ ..] if !path.is_empty() => {
            let path = path.join("/");
            let directory = if *kind == "blob" {
                if path == "SKILL.md" {
                    String::new()
                } else {
                    path.strip_suffix("/SKILL.md")
                        .ok_or("Expected a SKILL.md source.")?
                        .into()
                }
            } else {
                path
            };
            (
                resolve_export_commit(client, &api, reference).await?,
                Some(directory),
            )
        }
        [] => {
            let version = entry.version.as_str();
            let commit = if version == "unknown" || version.is_empty() {
                resolve_export_commit(client, &api, "HEAD").await?
            } else {
                match resolve_export_commit(client, &api, version).await {
                    Ok(commit) => commit,
                    Err(error) if !version.starts_with('v') => {
                        resolve_export_commit(client, &api, &format!("v{version}"))
                            .await
                            .map_err(|_| error)?
                    }
                    Err(error) => return Err(error),
                }
            };
            (commit, None)
        }
        _ => return Err("Unsupported GitHub skill URL.".into()),
    };
    let tree: Tree = get(client, format!("{api}/git/trees/{commit}?recursive=1")).await?;
    let paths = export_skill_paths(tree, explicit_directory.as_deref(), &entry.id)?;
    let raw = format!("https://raw.githubusercontent.com/{owner}/{repo}/{commit}");
    let mut files = BTreeMap::new();
    let mut bytes = 0;
    for batch in paths.chunks(8) {
        let mut downloads = tokio::task::JoinSet::new();
        for (relative, path) in batch {
            let url = format!("{raw}/{path}");
            let client = client.clone();
            let relative = relative.clone();
            downloads.spawn(async move {
                read_export_text(&client, url)
                    .await
                    .map(|text| (relative, text))
            });
        }
        while let Some(download) = downloads.join_next().await {
            let (path, text) = download.map_err(|error| error.to_string())??;
            bytes += text.len();
            if bytes > MAX_RESPONSE_BYTES {
                return Err("The skill exceeds the export size limit.".into());
            }
            files.insert(path, text);
        }
    }
    Ok(files)
}

async fn resolve_export_commit(
    client: &Client,
    api: &str,
    reference: &str,
) -> Result<String, String> {
    if valid_sha(reference) {
        return Ok(reference.into());
    }
    if !valid_export_path(reference) {
        return Err("Invalid GitHub revision.".into());
    }
    let commit: Commit = get(client, format!("{api}/commits/{reference}")).await?;
    valid_sha(&commit.sha)
        .then_some(commit.sha)
        .ok_or_else(|| "Invalid GitHub revision.".into())
}

pub(crate) fn valid_export_path(path: &str) -> bool {
    path.split('/').all(|part| {
        !matches!(part, "" | "." | "..")
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

fn export_skill_paths(
    tree: Tree,
    directory: Option<&str>,
    id: &str,
) -> Result<Vec<(String, String)>, String> {
    if tree.truncated {
        return Err("Incomplete GitHub skill listing.".into());
    }
    let regular = |entry: &&TreeEntry| matches!(entry.mode.as_str(), "100644" | "100755");
    let root = match directory {
        Some(directory) => directory,
        None if tree
            .tree
            .iter()
            .filter(regular)
            .any(|entry| entry.path == "SKILL.md") =>
        {
            ""
        }
        None => {
            let candidates: Vec<_> = tree
                .tree
                .iter()
                .filter(regular)
                .filter_map(|entry| entry.path.strip_suffix("/SKILL.md"))
                .collect();
            match candidates
                .iter()
                .find(|path| path.rsplit('/').next() == Some(id))
            {
                Some(path) => path,
                None if candidates.len() == 1 => candidates[0],
                _ => return Err("Could not identify a unique SKILL.md in this repository.".into()),
            }
        }
    };
    let prefix = format!("{root}/");
    let paths: Vec<_> = tree
        .tree
        .iter()
        .filter(regular)
        .filter_map(|entry| {
            let path = if root.is_empty() {
                let supported = entry.path == "SKILL.md"
                    || matches!(
                        entry.path.split('/').next(),
                        Some("references" | "scripts" | "assets" | "rules" | "examples")
                    )
                    || matches!(
                        entry.path.as_str(),
                        "LICENSE" | "LICENSE.md" | "LICENSE.txt"
                    );
                supported.then_some(entry.path.as_str())?
            } else {
                entry.path.strip_prefix(&prefix)?
            };
            valid_export_path(path).then(|| (path.to_owned(), entry.path.clone()))
        })
        .collect();
    if !paths.iter().any(|(relative, _)| relative == "SKILL.md") || paths.len() > 256 {
        return Err("The skill directory has no SKILL.md or exceeds the export file limit.".into());
    }
    Ok(paths)
}

#[cfg(test)]
#[test]
fn export_preserves_skill_subdirectories() {
    let tree: Tree = serde_json::from_value(serde_json::json!({
        "truncated": false,
        "tree": [
            {"path":"skills/review/SKILL.md", "mode":"100644", "sha":""},
            {"path":"skills/review/references/checks.md", "mode":"100644", "sha":""},
            {"path":"skills/review/link", "mode":"120000", "sha":""},
            {"path":"skills/other/SKILL.md", "mode":"100644", "sha":""}
        ]
    }))
    .unwrap();
    assert_eq!(
        export_skill_paths(tree, Some("skills/review"), "review").unwrap(),
        [
            ("SKILL.md".into(), "skills/review/SKILL.md".into()),
            (
                "references/checks.md".into(),
                "skills/review/references/checks.md".into()
            ),
        ]
    );
}

async fn read_export_text(client: &Client, url: String) -> Result<String, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len() + chunk.len() > 512 * 1024 {
            return Err("A skill file exceeds the export size limit.".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes)
        .map_err(|_| "The skill contains a binary file that the text export cannot include.".into())
}

async fn list_source_with_target(
    client: &Client,
    source: &str,
    target: Option<&str>,
) -> Result<BTreeMap<String, Skill>, String> {
    let root = format!("https://api.github.com/repos/{source}");
    let commit: Commit = get(client, format!("{root}/commits/HEAD")).await?;
    if !valid_sha(&commit.sha) {
        return Err("Invalid GitHub revision.".into());
    }
    let tree = get(
        client,
        format!("{root}/git/trees/{}?recursive=1", commit.sha),
    )
    .await?;
    let entries = skill_entries(source, commit.sha, tree, target)?;
    if target.is_some() && entries.len() > 1 {
        return Err("Multiple GitHub skill paths match the search result.".into());
    }
    Ok(entries)
}

fn update_snapshot(cache: &mut Cache, source: &str, skills: BTreeMap<String, Skill>) {
    let snapshot = cache
        .sources
        .entry(source.into())
        .or_insert_with(|| Snapshot {
            current: Vec::new(),
            skills: BTreeMap::new(),
        });
    snapshot.current = skills.keys().cloned().collect();
    for (id, skill) in skills {
        snapshot.skills.entry(id).or_insert(skill);
    }
}

#[tauri::command]
pub async fn sync_skill_catalog(force: bool) -> Result<bool, String> {
    let Some(path) = CACHE_PATH.get() else {
        return Err("Could not locate the local skills catalog.".into());
    };
    let _sync = SYNC_LOCK.lock().await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs();
    let mut cache = read_cache(path);
    if !force && now.saturating_sub(cache.last_attempt) < REFRESH_INTERVAL {
        return Ok(false);
    }
    cache.last_attempt = now;
    let client = crate::network::client()
        .user_agent("Prelude/0.1 skill catalog")
        .timeout(Duration::from_secs(7))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let jobs: Vec<_> = SOURCES
        .iter()
        .map(|&source| {
            let client = client.clone();
            (
                source,
                tauri::async_runtime::spawn(async move { list_source(&client, source).await }),
            )
        })
        .collect();
    let mut updated = false;
    for (source, job) in jobs {
        if let Ok(Ok(skills)) = job.await {
            update_snapshot(&mut cache, source, skills);
            updated = true;
        }
    }
    write_cache(path, &cache)?;
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_matches_the_skill_slug_in_a_nested_github_path() {
        let response: SearchResults = serde_json::from_str(r#"{
            "skills": [{"id":"acme/example/review","source":"acme/example","skillId":"review","name":"Review","installs":1500}]
        }"#).unwrap();
        let hit = &response.skills[0];
        let tree = Tree {
            truncated: false,
            tree: vec![
                TreeEntry {
                    path: "packages/tools/review/SKILL.md".into(),
                    mode: "100644".into(),
                    sha: "a".repeat(40),
                },
                TreeEntry {
                    path: "packages/tools/test/SKILL.md".into(),
                    mode: "100644".into(),
                    sha: "b".repeat(40),
                },
            ],
        };
        let matches =
            skill_entries(&hit.source, "c".repeat(40), tree, Some(&hit.skill_id)).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches.values().next().unwrap().path,
            "packages/tools/review/SKILL.md"
        );
    }

    fn listing(commit: char, directory_revision: char) -> BTreeMap<String, Skill> {
        let tree = Tree {
            truncated: false,
            tree: vec![
                TreeEntry {
                    path: "skills/review".into(),
                    mode: "040000".into(),
                    sha: directory_revision.to_string().repeat(40),
                },
                TreeEntry {
                    path: "skills/review/SKILL.md".into(),
                    mode: "100644".into(),
                    sha: "1".repeat(40),
                },
                TreeEntry {
                    path: "skills/linked/SKILL.md".into(),
                    mode: "120000".into(),
                    sha: "2".repeat(40),
                },
                TreeEntry {
                    path: "skills/nested/wrong/SKILL.md".into(),
                    mode: "100644".into(),
                    sha: "3".repeat(40),
                },
            ],
        };
        skill_entries(
            "obra/superpowers",
            commit.to_string().repeat(40),
            tree,
            None,
        )
        .unwrap()
    }

    #[test]
    fn preserves_pinned_skill_after_a_supporting_file_changes() {
        let mut cache = Cache::default();
        let source = "obra/superpowers";
        let original = listing('a', '4');
        assert_eq!(original.len(), 1);
        let id = original.first_key_value().unwrap().0.clone();
        update_snapshot(&mut cache, source, original);
        update_snapshot(&mut cache, source, listing('b', '5'));
        let snapshot = &cache.sources[source];
        assert_eq!(snapshot.current.len(), 1);
        assert_eq!(snapshot.skills.len(), 2);
        assert_ne!(snapshot.current[0], id);
        let archived = cache_entry(source, &id, &snapshot.skills[&id], false);
        assert!(archived
            .source
            .contains(&format!("/blob/{}/", "a".repeat(40))));
        assert!(is_archived(&archived));
    }
}
