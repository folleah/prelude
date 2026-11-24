use crate::{
    delivery::generate_build_files,
    domain::Plan,
    workspace::{encode_project_yaml, ProjectYaml, PROJECT_FILE},
};
use ring::digest::{digest, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Keep,
    Replace,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePreview {
    path: String,
    status: &'static str,
    diff: String,
    resolution: Option<Resolution>,
    #[serde(skip)]
    content: String,
    #[serde(skip)]
    original_hash: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    fingerprint: String,
    files: Vec<FilePreview>,
    ready: bool,
    setup_required: Vec<String>,
    #[serde(skip)]
    root: Option<PathBuf>,
}

pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    metadata.file_type().is_symlink()
}

pub(crate) fn read_existing(root: &Path, relative: &str) -> Result<Option<Vec<u8>>, String> {
    let relative_path = Path::new(relative);
    if !relative_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(format!("Unsafe path: {relative}"));
    }
    let mut current = root.to_owned();
    let mut components = relative_path.components().peekable();
    while let Some(component) = components.next() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link(&metadata) => {
                return Err(format!("Path contains a link: {relative}"))
            }
            Ok(metadata) if components.peek().is_some() && !metadata.is_dir() => {
                return Err(format!("Expected a directory: {relative}"))
            }
            Ok(metadata) if components.peek().is_none() && !metadata.is_file() => {
                return Err(format!("Expected a regular file: {relative}"))
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{relative}: {e}")),
        }
    }
    let mut bytes = Vec::new();
    fs::File::open(&current)
        .and_then(|file| file.take(1_048_577).read_to_end(&mut bytes))
        .map_err(|e| format!("{relative}: {e}"))?;
    if bytes.len() > 1_048_576 {
        return Err(format!("File is too large to preview: {relative}"));
    }
    Ok(Some(bytes))
}

fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn diff(original: &str, generated: &str) -> String {
    let old: Vec<_> = original.split_inclusive('\n').collect();
    let new: Vec<_> = generated.split_inclusive('\n').collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let mut result = String::new();
    for (mark, lines) in [
        (" ", &old[..prefix]),
        ("-", &old[prefix..old.len() - suffix]),
        ("+", &new[prefix..new.len() - suffix]),
        (" ", &new[new.len() - suffix..]),
    ] {
        for line in lines {
            result.push_str(mark);
            result.push_str(line);
            if !line.ends_with('\n') {
                result.push('\n');
            }
        }
    }
    result
}

#[tauri::command]
pub fn preview_configuration(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
    repository: Option<PathBuf>,
    resolutions: BTreeMap<String, Resolution>,
    workspace: Option<ProjectYaml>,
) -> Result<Preview, String> {
    let plan_bytes = serde_json::to_vec(&plan).map_err(|e| e.to_string())?;
    let root = repository
        .map(|path| {
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !metadata.is_dir() || is_link(&metadata) {
                return Err("The project root must be a regular directory.".to_string());
            }
            fs::canonicalize(path).map_err(|e| e.to_string())
        })
        .transpose()?;
    let rules_path = crate::delivery::rules_path(&plan)?;
    let build = generate_build_files(plan, extension_paths)?;
    let mut generated = build.files;
    if let Some(workspace) = workspace {
        if serde_json::to_vec(&workspace.plan).map_err(|e| e.to_string())? != plan_bytes {
            return Err("The workspace and configuration plans differ.".into());
        }
        if !workspace.description.trim().is_empty() {
            let rules = generated
                .get_mut(rules_path)
                .expect("generated project instructions");
            rules.insert_str(
                0,
                &format!("## Project brief\n\n{}\n\n", workspace.description.trim()),
            );
        }
        generated.insert(PROJECT_FILE.into(), encode_project_yaml(workspace)?);
    }
    assemble_preview(
        generated,
        &plan_bytes,
        root,
        resolutions,
        build.setup_required,
    )
}

fn assemble_preview(
    generated: BTreeMap<String, String>,
    plan_bytes: &[u8],
    root: Option<PathBuf>,
    resolutions: BTreeMap<String, Resolution>,
    setup_required: Vec<String>,
) -> Result<Preview, String> {
    let mut manifest = Vec::new();
    let mut files = Vec::new();
    let mut ready = true;
    for (path, content) in generated {
        let original = root
            .as_deref()
            .map(|root| read_existing(root, &path))
            .transpose()?
            .flatten();
        let resolution = resolutions.get(&path).copied();
        let (status, difference) = match &original {
            Some(bytes) if bytes == content.as_bytes() => ("unchanged", String::new()),
            Some(bytes) => {
                if resolution.is_none() {
                    ready = false;
                }
                let difference = std::str::from_utf8(bytes).map_or_else(
                    |_| "The existing file is not UTF-8 text.".into(),
                    |original| diff(original, &content),
                );
                ("conflict", difference)
            }
            None => ("new", diff("", &content)),
        };
        let resolution = resolution.filter(|_| status == "conflict");
        manifest.push((
            path.clone(),
            original.as_deref().map(hash),
            hash(content.as_bytes()),
            resolution,
        ));
        files.push(FilePreview {
            path,
            status,
            diff: difference,
            resolution,
            content,
            original_hash: original.as_deref().map(hash),
        });
    }
    let serialized =
        serde_json::to_vec(&(hash(plan_bytes), &root, manifest)).map_err(|e| e.to_string())?;
    Ok(Preview {
        fingerprint: hash(&serialized),
        files,
        ready,
        setup_required,
        root,
    })
}

#[tauri::command]
pub fn check_preview(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
    repository: Option<PathBuf>,
    resolutions: BTreeMap<String, Resolution>,
    fingerprint: String,
    workspace: Option<ProjectYaml>,
) -> Result<(), String> {
    let current = preview_configuration(plan, extension_paths, repository, resolutions, workspace)?;
    validate_preview(&current, &fingerprint)
}

fn validate_preview(current: &Preview, fingerprint: &str) -> Result<(), String> {
    if current.fingerprint != fingerprint {
        return Err("The plan or files have changed. Refresh the preview.".into());
    }
    if !current.ready {
        return Err("Choose an action for each conflict.".into());
    }
    Ok(())
}

const JOURNAL: &str = ".makemeharness-recovery";

#[derive(Serialize)]
struct JournalFile {
    path: String,
    original_sha256: Option<String>,
    generated_sha256: String,
    backup: Option<String>,
}

fn digest_in(dir: &cap_std::fs::Dir, path: &Path) -> Result<Option<String>, String> {
    let mut bytes = Vec::new();
    match dir.open(path) {
        Ok(file) => {
            file.take(1_048_577)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if bytes.len() > 1_048_576 {
                return Err("The destination file is too large.".into());
            }
            Ok(Some(hash(&bytes)))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_new(dir: &cap_std::fs::Dir, name: &str, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = dir.open_with(name, &options)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[tauri::command]
pub fn apply_configuration(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
    repository: PathBuf,
    resolutions: BTreeMap<String, Resolution>,
    fingerprint: String,
    workspace: Option<ProjectYaml>,
) -> Result<(), String> {
    let preview = preview_configuration(
        plan,
        extension_paths,
        Some(repository),
        resolutions,
        workspace,
    )?;
    validate_preview(&preview, &fingerprint)?;
    apply_preview(preview)
}

fn apply_preview(preview: Preview) -> Result<(), String> {
    let root = preview.root.ok_or("Select the repository directory.")?;
    let dir = cap_std::fs::Dir::open_ambient_dir(&root, cap_std::ambient_authority())
        .map_err(|e| e.to_string())?;
    let files: Vec<_> = preview
        .files
        .into_iter()
        .filter(|file| file.status != "unchanged" && file.resolution != Some(Resolution::Keep))
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    dir.create_dir(JOURNAL).map_err(|e| {
        format!(
            "Could not create journal directory {}: {e}",
            root.join(JOURNAL).display()
        )
    })?;
    let journal = dir.open_dir(JOURNAL).map_err(|e| e.to_string())?;
    let mut created_dirs = Vec::new();
    let mut committed = Vec::new();
    let mut needs_recovery = false;
    let result = (|| -> Result<(), String> {
        let mut manifest = Vec::new();
        for (index, file) in files.iter().enumerate() {
            let original = read_existing(&root, &file.path)?;
            if original.as_deref().map(hash) != file.original_hash {
                return Err(format!("File changed after preview: {}", file.path));
            }
            let backup = original
                .map(|bytes| {
                    let name = format!("{index}.original");
                    write_new(&journal, &name, &bytes).map(|_| name)
                })
                .transpose()
                .map_err(|e| e.to_string())?;
            write_new(
                &journal,
                &format!("{index}.pending"),
                file.content.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
            manifest.push(JournalFile {
                path: file.path.clone(),
                original_sha256: file.original_hash.clone(),
                generated_sha256: hash(file.content.as_bytes()),
                backup,
            });
        }
        let manifest = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        write_new(&journal, "manifest.json", &manifest).map_err(|e| e.to_string())?;
        for (index, item) in files.iter().enumerate() {
            let path = Path::new(&item.path);
            read_existing(&root, &item.path)?;
            let mut current = PathBuf::new();
            for part in path.parent().into_iter().flat_map(Path::components) {
                current.push(part.as_os_str());
                match dir.create_dir(&current) {
                    Ok(()) => created_dirs.push(current.clone()),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            if digest_in(&dir, path)? != item.original_hash {
                return Err(format!("File changed after preview: {}", item.path));
            }
            let staged = format!("{index}.pending");
            if item.original_hash.is_some() {
                let displaced = format!("{index}.displaced");
                dir.rename(path, &journal, &displaced)
                    .map_err(|e| e.to_string())?;
                #[cfg(test)]
                if std::env::var_os("MAKEMEHARNESS_TEST_INTERRUPT").is_some() {
                    std::process::exit(73);
                }
                let install = (|| -> Result<(), String> {
                    let metadata = journal
                        .symlink_metadata(&displaced)
                        .map_err(|e| e.to_string())?;
                    if !metadata.is_file()
                        || digest_in(&journal, Path::new(&displaced))? != item.original_hash
                    {
                        return Err(format!("File was replaced during the write: {}", item.path));
                    }
                    journal
                        .hard_link(&staged, &dir, path)
                        .map_err(|e| e.to_string())
                })();
                if let Err(error) = install {
                    // A competing writer may already have filled the target. Keep both
                    // versions in that case and leave recovery to the user.
                    needs_recovery = journal.hard_link(&displaced, &dir, path).is_err();
                    return Err(error);
                }
            } else {
                journal
                    .hard_link(&staged, &dir, path)
                    .map_err(|e| e.to_string())?;
            }
            committed.push(index);
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut restored = !needs_recovery;
        for index in committed.into_iter().rev() {
            let item = &files[index];
            let path = Path::new(&item.path);
            if read_existing(&root, &item.path).is_err()
                || digest_in(&dir, path).ok().flatten().as_deref()
                    != Some(hash(item.content.as_bytes()).as_str())
            {
                restored = false;
                continue;
            }
            let rollback = if item.original_hash.is_some() {
                journal.rename(format!("{index}.original"), &dir, path)
            } else {
                dir.remove_file(path)
            };
            restored &= rollback.is_ok();
        }
        for path in created_dirs.into_iter().rev() {
            let _ = dir.remove_dir(path);
        }
        if !restored {
            return Err(format!(
                "{error}\nUse the journal to recover: {}",
                root.join(JOURNAL).display()
            ));
        }
        clear_journal(&dir, journal, files.len())?;
        return Err(error);
    }
    clear_journal(&dir, journal, files.len())
}

fn clear_journal(
    dir: &cap_std::fs::Dir,
    journal: cap_std::fs::Dir,
    count: usize,
) -> Result<(), String> {
    for index in 0..count {
        for suffix in ["original", "pending", "displaced"] {
            match journal.remove_file(format!("{index}.{suffix}")) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("Could not clean up journal {JOURNAL}: {error}")),
            }
        }
    }
    match journal.remove_file("manifest.json") {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not clean up journal {JOURNAL}: {error}")),
    }
    drop(journal);
    dir.remove_dir(JOURNAL)
        .map_err(|e| format!("Could not clean up journal {JOURNAL}: {e}"))
}

#[tauri::command]
pub async fn pick_project_directory(
    window: tauri::WebviewWindow,
    current: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = window
        .dialog()
        .file()
        .set_parent(&window)
        .set_title("Select project directory");
    if let Some(path) = current.filter(|path| path.is_dir()) {
        dialog = dialog.set_directory(path);
    }
    dialog
        .blocking_pick_folder()
        .map(|path| path.into_path().map_err(|error| error.to_string()))
        .transpose()
}

#[tauri::command]
pub fn validate_project_directory(
    repository: PathBuf,
    require_empty: Option<bool>,
) -> Result<(), String> {
    if !repository.is_absolute() {
        return Err("Select a directory with a full path.".into());
    }
    let root = if require_empty.unwrap_or(true) {
        empty_project_directory(&repository)?
    } else {
        existing_project_directory(&repository)?
    };
    check_directory_write_access(&root)
}

fn check_directory_write_access(root: &Path) -> Result<(), String> {
    let probe = root.join(format!(
        ".makemeharness-write-test-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|error| format!("Cannot write to the selected directory: {error}"))?;
    drop(file);
    fs::remove_file(&probe).map_err(|error| {
        format!(
            "Cannot remove the temporary file {}: {error}",
            probe.display()
        )
    })
}

pub(crate) fn existing_project_directory(repository: &std::path::Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(repository).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err("Select an existing regular directory.".into());
    }
    fs::canonicalize(repository).map_err(|e| e.to_string())
}

fn empty_project_directory(repository: &std::path::Path) -> Result<PathBuf, String> {
    let root = existing_project_directory(repository)?;
    if fs::read_dir(&root)
        .map_err(|e| e.to_string())?
        .next()
        .is_some()
    {
        return Err("The project directory must be empty.".into());
    }
    Ok(root)
}

#[tauri::command]
pub fn create_project(
    plan: Plan,
    extension_paths: Vec<PathBuf>,
    repository: PathBuf,
    fingerprint: String,
    workspace: Option<ProjectYaml>,
) -> Result<(), String> {
    use std::io::Write;

    let root = empty_project_directory(&repository)?;
    let preview = preview_configuration(
        plan,
        extension_paths,
        Some(root.clone()),
        BTreeMap::new(),
        workspace,
    )?;
    validate_preview(&preview, &fingerprint)?;
    let mut created = Vec::new();
    let result = (|| -> Result<(), String> {
        for file in preview.files {
            if file.status != "new" {
                return Err("The project directory is no longer empty.".into());
            }
            read_existing(&root, &file.path)?;
            let target = root.join(&file.path);
            let mut parent = root.clone();
            for part in Path::new(&file.path)
                .parent()
                .into_iter()
                .flat_map(Path::components)
            {
                parent.push(part.as_os_str());
                if created
                    .iter()
                    .any(|(path, is_dir)| *is_dir && path == &parent)
                {
                    continue;
                }
                fs::create_dir(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
                created.push((parent.clone(), true));
            }
            read_existing(&root, &file.path)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
            created.push((target, false));
            output
                .write_all(file.content.as_bytes())
                .and_then(|_| output.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut remaining = Vec::new();
        for (path, is_dir) in created.into_iter().rev() {
            let cleanup = if is_dir {
                fs::remove_dir(&path)
            } else {
                fs::remove_file(&path)
            };
            if cleanup.is_err() {
                remaining.push(path.display().to_string());
            }
        }
        if !remaining.is_empty() {
            return Err(format!(
                "{error}\nCheck the remaining paths: {}",
                remaining.join(", ")
            ));
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_preview(
        plan: Plan,
        paths: Vec<PathBuf>,
        repository: Option<PathBuf>,
        resolutions: BTreeMap<String, Resolution>,
        fingerprint: String,
    ) -> Result<(), String> {
        super::check_preview(plan, paths, repository, resolutions, fingerprint, None)
    }

    fn preview_configuration(
        plan: Plan,
        paths: Vec<PathBuf>,
        repository: Option<PathBuf>,
        resolutions: BTreeMap<String, Resolution>,
    ) -> Result<Preview, String> {
        super::preview_configuration(plan, paths, repository, resolutions, None)
    }

    fn create_project(
        plan: Plan,
        paths: Vec<PathBuf>,
        repository: PathBuf,
        fingerprint: String,
    ) -> Result<(), String> {
        super::create_project(plan, paths, repository, fingerprint, None)
    }

    fn apply_configuration(
        plan: Plan,
        paths: Vec<PathBuf>,
        repository: PathBuf,
        resolutions: BTreeMap<String, Resolution>,
        fingerprint: String,
    ) -> Result<(), String> {
        super::apply_configuration(plan, paths, repository, resolutions, fingerprint, None)
    }
    use serde_json::json;

    fn plan(instruction: &str) -> Plan {
        serde_json::from_value(json!({
            "harness": {"id":"codex","version":"0.167.1","reason":""},
            "components": [], "roles": [{"name":"developer","responsibility":"Implement","interaction":"Report","reason":""}],
            "instructions": [instruction]
        })).unwrap()
    }

    #[test]
    fn builds_blog_with_brief_and_skill_guidance() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let mut blog = serde_json::to_value(plan("Verify the blog at mobile widths.")).unwrap();
        blog["components"] = json!([
            {"id":"humanizer","version":"4.0.0","reason":"Public copy"},
            {"id":"web-design-guidelines","version":"unknown","reason":"Interface design"},
            {"id":"webapp-testing","version":"unknown","reason":"Browser validation"}
        ]);
        let workspace = || {
            serde_json::from_value(json!({
                "schemaVersion": 1,
                "description": "Build a small gardening blog with articles and a homepage.",
                "plan": blog,
            }))
            .unwrap()
        };
        let blog_plan = || serde_json::from_value(blog.clone()).unwrap();
        let preview = super::preview_configuration(
            blog_plan(),
            vec![],
            Some(root.clone()),
            BTreeMap::new(),
            Some(workspace()),
        )
        .unwrap();
        super::create_project(
            blog_plan(),
            vec![],
            root.clone(),
            preview.fingerprint,
            Some(workspace()),
        )
        .unwrap();
        let instructions = fs::read_to_string(root.join("AGENTS.md")).unwrap();
        assert!(instructions.contains("gardening blog"));
        assert!(instructions.contains("Humanizer"));
        assert!(instructions.contains("Vercel Web Design Guidelines"));
        assert!(instructions.contains("Anthropic Web App Testing"));
        let yaml = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert!(crate::workspace::decode_project_yaml(yaml).is_ok());
        if std::env::var_os("PRELUDE_CODEX_SMOKE_FIXTURE").is_some() {
            println!("Blog fixture: {}", root.display());
        } else {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn validated_saved_plan_creates_and_updates_projects_without_network() {
        let saved = serde_json::to_vec(&plan("Run tests offline")).unwrap();
        let reopen = || serde_json::from_slice::<Plan>(&saved).unwrap();
        assert!(crate::validation::validate_plan(reopen(), vec![])
            .unwrap()
            .is_empty());
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let first =
            preview_configuration(reopen(), vec![], Some(root.clone()), BTreeMap::new()).unwrap();
        create_project(reopen(), vec![], root.clone(), first.fingerprint).unwrap();
        let target = root.join("AGENTS.md");
        assert!(fs::read_to_string(&target)
            .unwrap()
            .contains("Run tests offline"));
        fs::write(&target, "Local edit").unwrap();
        let choices = BTreeMap::from([("AGENTS.md".into(), Resolution::Replace)]);
        let next =
            preview_configuration(reopen(), vec![], Some(root.clone()), choices.clone()).unwrap();
        apply_configuration(reopen(), vec![], root.clone(), choices, next.fingerprint).unwrap();
        assert!(fs::read_to_string(&target)
            .unwrap()
            .contains("Run tests offline"));
        fs::remove_file(target).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn applies_confirmed_replacement_and_removes_journal() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let target = root.join("AGENTS.md");
        fs::write(&target, "Original").unwrap();
        let resolutions = BTreeMap::from([("AGENTS.md".into(), Resolution::Replace)]);
        let preview = preview_configuration(
            plan("Test"),
            vec![],
            Some(root.clone()),
            resolutions.clone(),
        )
        .unwrap();
        let fingerprint = preview.fingerprint;
        fs::write(&target, "Changed").unwrap();
        assert!(apply_configuration(
            plan("Test"),
            vec![],
            root.clone(),
            resolutions.clone(),
            fingerprint.clone()
        )
        .is_err());
        assert_eq!(fs::read_to_string(&target).unwrap(), "Changed");
        fs::write(&target, "Original").unwrap();
        apply_configuration(plan("Test"), vec![], root.clone(), resolutions, fingerprint).unwrap();
        assert!(fs::read_to_string(&target).unwrap().contains("Test"));
        assert!(!root.join(JOURNAL).exists());
        fs::remove_file(target).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn existing_recovery_journal_blocks_new_write() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let journal = root.join(JOURNAL);
        fs::create_dir(&journal).unwrap();
        let backup = journal.join("0.original");
        fs::write(&backup, "Original").unwrap();
        let preview =
            preview_configuration(plan("Test"), vec![], Some(root.clone()), BTreeMap::new())
                .unwrap();
        let error = apply_configuration(
            plan("Test"),
            vec![],
            root.clone(),
            BTreeMap::new(),
            preview.fingerprint,
        )
        .unwrap_err();
        assert!(error.contains(JOURNAL));
        assert_eq!(fs::read_to_string(&backup).unwrap(), "Original");
        assert!(!root.join("AGENTS.md").exists());
        fs::remove_file(backup).unwrap();
        fs::remove_dir(journal).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn interrupted_replacement_preserves_original_and_manifest() {
        const INTERRUPT: &str = "MAKEMEHARNESS_TEST_INTERRUPT";
        if let Some(root) = std::env::var_os(INTERRUPT).map(PathBuf::from) {
            let choices = BTreeMap::from([("AGENTS.md".into(), Resolution::Replace)]);
            let snapshot =
                preview_configuration(plan("Test"), vec![], Some(root.clone()), choices.clone())
                    .unwrap();
            apply_configuration(plan("Test"), vec![], root, choices, snapshot.fingerprint).unwrap();
            panic!("Expected forced exit after moving the original");
        }
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        fs::write(root.join("AGENTS.md"), "Original").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "preview::tests::interrupted_replacement_preserves_original_and_manifest",
            ])
            .env(INTERRUPT, &root)
            .output()
            .unwrap()
            .status;
        assert_eq!(status.code(), Some(73));
        let journal = root.join(JOURNAL);
        assert_eq!(
            fs::read_to_string(journal.join("0.original")).unwrap(),
            "Original"
        );
        assert_eq!(
            fs::read_to_string(journal.join("0.displaced")).unwrap(),
            "Original"
        );
        assert!(fs::read_to_string(journal.join("manifest.json"))
            .unwrap()
            .contains("AGENTS.md"));
        assert!(!root.join("AGENTS.md").exists());
        fs::rename(journal.join("0.displaced"), root.join("AGENTS.md")).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            "Original"
        );
        for file in ["0.original", "0.pending", "manifest.json"] {
            fs::remove_file(journal.join(file)).unwrap();
        }
        fs::remove_dir(journal).unwrap();
        fs::remove_file(root.join("AGENTS.md")).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn rejects_project_root_replaced_by_directory_link() {
        let sandbox = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let root = sandbox.join("project");
        let outside = sandbox.join("outside");
        fs::create_dir(&sandbox).unwrap();
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let preview =
            preview_configuration(plan("Test"), vec![], Some(root.clone()), BTreeMap::new())
                .unwrap();
        fs::remove_dir(&root).unwrap();
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&root)
                .arg(&outside)
                .output()
                .unwrap();
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &root).unwrap();
        assert!(apply_configuration(
            plan("Test"),
            vec![],
            root.clone(),
            BTreeMap::new(),
            preview.fingerprint
        )
        .is_err());
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        #[cfg(windows)]
        fs::remove_dir(root).unwrap();
        #[cfg(unix)]
        fs::remove_file(root).unwrap();
        fs::remove_dir(outside).unwrap();
        fs::remove_dir(sandbox).unwrap();
    }

    #[test]
    fn creates_project_only_in_existing_empty_directory() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        validate_project_directory(root.clone(), None).unwrap();
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        let preview =
            preview_configuration(plan("Test"), vec![], Some(root.clone()), BTreeMap::new())
                .unwrap();
        create_project(
            plan("Test"),
            vec![],
            root.clone(),
            preview.fingerprint.clone(),
        )
        .unwrap();
        let target = root.join("AGENTS.md");
        assert!(validate_project_directory(root.clone(), None).is_err());
        validate_project_directory(root.clone(), Some(false)).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            preview.files[0].content
        );
        assert!(create_project(plan("Test"), vec![], root.clone(), preview.fingerprint).is_err());
        fs::remove_file(target).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn rejects_stale_files_and_plan_after_resolving_conflict() {
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&root).unwrap();
        let target = root.join("AGENTS.md");
        fs::write(&target, "Old instructions\n").unwrap();
        let repository = || Some(root.clone());
        let first =
            preview_configuration(plan("Test"), vec![], repository(), BTreeMap::new()).unwrap();
        assert!(!first.ready);
        assert!(first.files[0].diff.contains("-Old instructions"));
        let choices = BTreeMap::from([("AGENTS.md".into(), Resolution::Replace)]);
        let preview =
            preview_configuration(plan("Test"), vec![], repository(), choices.clone()).unwrap();
        assert!(preview.ready);
        assert!(check_preview(
            plan("Test"),
            vec![],
            repository(),
            choices.clone(),
            preview.fingerprint.clone()
        )
        .is_ok());
        assert!(check_preview(
            plan("Review"),
            vec![],
            repository(),
            choices.clone(),
            preview.fingerprint.clone()
        )
        .is_err());
        fs::write(&target, "Changed after preview\n").unwrap();
        assert!(check_preview(
            plan("Test"),
            vec![],
            repository(),
            choices,
            preview.fingerprint
        )
        .is_err());
        fs::remove_file(target).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
