use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrowserAction {
    None,
    InsertSkill(String),
    SelectRelease(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrowserItem {
    pub title: String,
    pub description: String,
    pub body: String,
    pub action: BrowserAction,
}

pub fn release_notes() -> Vec<BrowserItem> {
    vec![BrowserItem {
        title: "Indus v0.1.0".to_string(),
        description: "August 2026 · India's First AI Native CLI Released".to_string(),
        body: [
            "Indus v0.1.0",
            "August 2026",
            "",
            "India's First AI Native CLI Released",
            "",
            "Features",
            "• Streaming reasoning and responses",
            "• Built-in file, shell, web, and Jobs tools",
            "• Compatible Interim Provider and model catalogs",
            "• Persistent sessions with resume support",
            "• Planning and Always-Approve execution modes",
            "• Indus themes and terminal-native interaction",
        ]
        .join("\n"),
        action: BrowserAction::SelectRelease("0.1.0".to_string()),
    }]
}

pub fn mcp_catalog() -> Vec<BrowserItem> {
    vec![BrowserItem {
        title: "Exa Search".to_string(),
        description: "Connected · Built-in web search MCP".to_string(),
        body: [
            "Exa Search",
            "Status: Connected",
            "Transport: Streamable HTTP",
            "Endpoint: https://mcp.exa.ai/mcp",
            "",
            "Tools",
            "• web_search — search the public web and return source-backed results",
        ]
        .join("\n"),
        action: BrowserAction::None,
    }]
}

pub fn installed_skills(cwd: &Path) -> Vec<BrowserItem> {
    let mut files = Vec::new();
    for root in skill_roots(cwd) {
        collect_skill_files(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();

    let mut names = HashSet::new();
    let mut items = Vec::new();
    for path in files {
        let name = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "skill".to_string());
        if !names.insert(name.clone()) {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap_or_default();
        let description =
            skill_description(&source).unwrap_or_else(|| "Installed agent skill".to_string());
        items.push(BrowserItem {
            title: name.clone(),
            description,
            body: format!(
                "{}\n\nSource\n{}\n\nPress Enter to insert this skill into the composer.",
                name,
                path.display()
            ),
            action: BrowserAction::InsertSkill(name),
        });
    }
    items.sort_by(|left, right| left.title.cmp(&right.title));
    items
}

pub fn installed_workflows(cwd: &Path) -> Vec<BrowserItem> {
    let mut files = Vec::new();
    for root in workflow_roots(cwd) {
        collect_markdown_files(&root, 0, &mut files);
    }
    files.sort();
    files.dedup();
    files
        .into_iter()
        .filter_map(|path| {
            let source = fs::read_to_string(&path).ok()?;
            let title = source
                .lines()
                .find_map(|line| line.trim().strip_prefix("# "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    path.file_stem()
                        .map(|name| name.to_string_lossy().replace(['-', '_'], " "))
                })?;
            let description = source
                .lines()
                .map(str::trim)
                .find(|line| {
                    !line.is_empty()
                        && !line.starts_with('#')
                        && !line.starts_with("---")
                        && !line.starts_with("name:")
                        && !line.starts_with("description:")
                })
                .unwrap_or("Saved Indus workflow")
                .chars()
                .take(100)
                .collect();
            Some(BrowserItem {
                title,
                description,
                body: format!("{}\n\nSource\n{}", source.trim(), path.display()),
                action: BrowserAction::None,
            })
        })
        .collect()
}

pub fn prompt_skill_instructions(prompt: &str, cwd: &Path) -> Vec<String> {
    let requested = prompt
        .split_whitespace()
        .filter_map(|token| token.strip_prefix('$'))
        .map(|name| {
            name.trim_matches(|character: char| {
                !character.is_alphanumeric() && !matches!(character, '-' | '_' | ':')
            })
        })
        .filter(|name| !name.is_empty())
        .collect::<HashSet<_>>();
    if requested.is_empty() {
        return Vec::new();
    }

    let mut files = Vec::new();
    for root in skill_roots(cwd) {
        collect_skill_files(&root, 0, &mut files);
    }
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let name = path.parent()?.file_name()?.to_string_lossy();
            requested.contains(name.as_ref()).then(|| {
                fs::read_to_string(&path).ok().map(|source| {
                    format!(
                        "Apply the following selected skill instructions for this turn. User instructions take precedence.\n<skill name=\"{name}\">\n{source}\n</skill>"
                    )
                })
            })?
        })
        .take(5)
        .collect()
}

fn skill_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![cwd.join(".indus/skills"), cwd.join(".agents/skills")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join(".agents/skills"));
        roots.push(home.join(".codex/skills"));
    }
    roots
}

fn workflow_roots(cwd: &Path) -> Vec<PathBuf> {
    let mut roots = vec![cwd.join(".indus/workflows")];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join(".indus/workflows"));
    }
    roots
}

fn collect_markdown_files(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 4 || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, depth + 1, output);
        } else if path.extension().is_some_and(|extension| extension == "md") {
            output.push(path);
        }
    }
}

fn collect_skill_files(root: &Path, depth: usize, output: &mut Vec<PathBuf>) {
    if depth > 4 || !root.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_skill_files(&path, depth + 1, output);
        } else if path.file_name().is_some_and(|name| name == "SKILL.md") {
            output.push(path);
        }
    }
}

fn skill_description(source: &str) -> Option<String> {
    let value = source.lines().find_map(|line| {
        line.trim()
            .strip_prefix("description:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })?;
    Some(value.trim_matches(['\'', '"']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_catalog_starts_with_indus_version_one() {
        let notes = release_notes();
        assert_eq!(notes[0].title, "Indus v0.1.0");
        assert!(
            notes[0]
                .body
                .contains("India's First AI Native CLI Released")
        );
    }

    #[test]
    fn skill_descriptions_are_read_from_frontmatter() {
        assert_eq!(
            skill_description("---\ndescription: 'Review Rust code'\n---").as_deref(),
            Some("Review Rust code")
        );
    }

    #[test]
    fn prompt_skill_names_ignore_surrounding_punctuation() {
        let requested = "$rust-review, then continue";
        let names = requested
            .split_whitespace()
            .filter_map(|token| token.strip_prefix('$'))
            .map(|name| {
                name.trim_matches(|character: char| {
                    !character.is_alphanumeric() && !matches!(character, '-' | '_' | ':')
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["rust-review"]);
    }
}
