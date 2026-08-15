//! Built-in tools used by the Indus coding harness.

use std::{
    collections::BTreeMap,
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{Value, json};
use similar::{ChangeTag, TextDiff};

use super::{
    event::{DiffKind, DiffLine, FileDiff},
    jobs::{JobSchedule, JobService},
    model::ToolDefinition,
    tool::{HarnessTool, ToolContext, ToolError, ToolOutput, ToolPermission, ToolRegistry},
};

const MAX_TOOL_OUTPUT: usize = 50 * 1024;
const MAX_READ_LINES: usize = 2_000;
const MAX_LINE_BYTES: usize = 2_000;
const MAX_WEB_BYTES: usize = 5 * 1024 * 1024;

pub fn registry(jobs: JobService) -> ToolRegistry {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let registry = ToolRegistry::default();
    registry.register(ReadTool { cwd: cwd.clone() });
    registry.register(GlobTool { cwd: cwd.clone() });
    registry.register(GrepTool { cwd: cwd.clone() });
    registry.register(ShellTool { cwd: cwd.clone() });
    registry.register(WriteTool { cwd: cwd.clone() });
    registry.register(EditTool { cwd: cwd.clone() });
    registry.register(ApplyPatchTool { cwd: cwd.clone() });
    registry.register(WebFetchTool::default());
    registry.register(WebSearchTool::default());
    registry.register(RepoCloneTool { cwd: cwd.clone() });
    registry.register(RepoOverviewTool { cwd });
    registry.register(TodoTool::default());
    registry.register(JobTool { jobs });
    registry
}

#[derive(Clone)]
struct ReadTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct ReadInput {
    #[serde(alias = "filePath", alias = "path")]
    file_path: String,
    #[serde(default = "one")]
    offset: usize,
    #[serde(default = "default_read_limit")]
    limit: usize,
}

impl HarnessTool for ReadTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "read",
            "Read a text file with line numbers, or list a directory. Output is bounded and binary files are rejected.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string", "description": "Absolute path or path relative to the working directory" },
                    "offset": { "type": "integer", "minimum": 1 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 2000 }
                },
                "required": ["file_path"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "read",
            input_path::<ReadInput>(input, |value| &value.file_path),
            "Read a file or directory",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: ReadInput = parse(input)?;
        context.check_cancelled()?;
        let path = resolve(&self.cwd, &input.file_path);
        if path.is_dir() {
            let mut entries = fs::read_dir(&path)
                .map_err(tool_error)?
                .filter_map(Result::ok)
                .map(|entry| {
                    let suffix = entry
                        .file_type()
                        .ok()
                        .filter(|kind| kind.is_dir())
                        .map(|_| "/")
                        .unwrap_or("");
                    format!("{}{suffix}", entry.file_name().to_string_lossy())
                })
                .collect::<Vec<_>>();
            entries.sort();
            let offset = input.offset.saturating_sub(1).min(entries.len());
            let limit = input.limit.clamp(1, MAX_READ_LINES);
            let end = offset.saturating_add(limit).min(entries.len());
            return Ok(ToolOutput {
                title: display_path(&self.cwd, &path),
                output: truncate_output(entries[offset..end].join("\n")),
                diffs: Vec::new(),
            });
        }
        let bytes = fs::read(&path).map_err(tool_error)?;
        if binary(&bytes) {
            return Err(ToolError::new(format!(
                "Cannot read binary file: {}",
                path.display()
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let offset = input.offset.saturating_sub(1).min(lines.len());
        let limit = input.limit.clamp(1, MAX_READ_LINES);
        let end = offset.saturating_add(limit).min(lines.len());
        let mut output = Vec::new();
        for (index, line) in lines[offset..end].iter().enumerate() {
            output.push(format!(
                "{:>6}\t{}",
                offset + index + 1,
                truncate_line(line)
            ));
        }
        if end < lines.len() {
            output.push(format!("\n(Output continues at line {}.)", end + 1));
        }
        Ok(ToolOutput {
            title: display_path(&self.cwd, &path),
            output: truncate_output(output.join("\n")),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct GlobTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct GlobInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
}

impl HarnessTool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "glob",
            "Find files matching a glob. Results are limited to 100 paths.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string", "description": "Directory to search; defaults to the working directory" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission("glob", input_value(input, "pattern"), "Find matching files")
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: GlobInput = parse(input)?;
        context.check_cancelled()?;
        let root = resolve(&self.cwd, input.path.as_deref().unwrap_or("."));
        let output = Command::new("rg")
            .args([
                "--files",
                "--hidden",
                "--glob",
                &input.pattern,
                "--glob",
                "!.git",
            ])
            .current_dir(&root)
            .output()
            .map_err(tool_error)?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(command_error("rg", &output.stderr));
        }
        let mut files: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .take(101)
            .map(|path| root.join(path).display().to_string())
            .collect();
        let truncated = files.len() > 100;
        files.truncate(100);
        if files.is_empty() {
            files.push("No files found".into());
        } else if truncated {
            files.push("\n(Results truncated at 100 files.)".into());
        }
        Ok(ToolOutput {
            title: input.pattern,
            output: files.join("\n"),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct GrepTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct GrepInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include: Option<String>,
}

impl HarnessTool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "grep",
            "Search file contents with a regular expression using ripgrep.",
            json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "include": { "type": "string", "description": "Optional file glob such as *.rs" }
                },
                "required": ["pattern"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "grep",
            input_value(input, "pattern"),
            "Search file contents",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: GrepInput = parse(input)?;
        context.check_cancelled()?;
        let target = resolve(&self.cwd, input.path.as_deref().unwrap_or("."));
        let (root, file) = if target.is_file() {
            (
                target.parent().unwrap_or(&self.cwd).to_path_buf(),
                Some(target.file_name().unwrap_or_default().to_owned()),
            )
        } else {
            (target, None)
        };
        let mut command = Command::new("rg");
        command.args([
            "--line-number",
            "--no-heading",
            "--color",
            "never",
            "--hidden",
            "--glob",
            "!.git",
        ]);
        if let Some(include) = &input.include {
            command.args(["--glob", include]);
        }
        command.arg(&input.pattern);
        if let Some(file) = file {
            command.arg(file);
        }
        command.current_dir(&root);
        let output = command.output().map_err(tool_error)?;
        if !output.status.success() && output.status.code() != Some(1) {
            return Err(command_error("rg", &output.stderr));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let rows: Vec<&str> = stdout.lines().take(101).collect();
        let truncated = rows.len() > 100;
        let mut rendered = rows
            .into_iter()
            .take(100)
            .map(truncate_line)
            .collect::<Vec<_>>()
            .join("\n");
        if rendered.is_empty() {
            rendered = "No matches found".into();
        }
        if truncated {
            rendered.push_str("\n\n(Results truncated at 100 matches.)");
        }
        Ok(ToolOutput {
            title: input.pattern,
            output: rendered,
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct ShellTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct ShellInput {
    command: String,
    #[serde(default)]
    description: Option<String>,
}

impl HarnessTool for ShellTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "shell",
            "Run a shell command in the working directory. Use for builds, tests, Git, and system operations.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "description": { "type": "string" }
                },
                "required": ["command"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "shell",
            input_value(input, "command"),
            "Run a shell command",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: ShellInput = parse(input)?;
        context.check_cancelled()?;
        let output = Command::new("bash")
            .args(["-lc", &input.command])
            .current_dir(&self.cwd)
            .output()
            .map_err(tool_error)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let rendered = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
            (false, false) => format!("{stdout}\n{stderr}"),
            (false, true) => stdout.into_owned(),
            (true, false) => stderr.into_owned(),
            (true, true) => format!("Command exited with status {}.", output.status),
        };
        let rendered = truncate_output(rendered);
        context.emit_output(rendered.clone());
        if !output.status.success() {
            return Err(ToolError::new(format!(
                "Command failed with {}\n{rendered}",
                output.status
            )));
        }
        Ok(ToolOutput {
            title: input
                .description
                .unwrap_or_else(|| command_title(&input.command)),
            output: rendered,
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct WriteTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct WriteInput {
    #[serde(alias = "filePath")]
    file_path: String,
    content: String,
}

impl HarnessTool for WriteTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "write",
            "Create or replace a file. Parent directories are created automatically and a structured diff is returned.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["file_path", "content"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "edit",
            input_path::<WriteInput>(input, |value| &value.file_path),
            "Write a file",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: WriteInput = parse(input)?;
        context.check_cancelled()?;
        let path = resolve(&self.cwd, &input.file_path);
        let old = fs::read_to_string(&path).unwrap_or_default();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(tool_error)?;
        }
        fs::write(&path, &input.content).map_err(tool_error)?;
        Ok(ToolOutput {
            title: display_path(&self.cwd, &path),
            output: "Wrote file successfully.".into(),
            diffs: vec![file_diff(&path, &old, &input.content)],
        })
    }
}

#[derive(Clone)]
struct EditTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct EditInput {
    #[serde(alias = "filePath")]
    file_path: String,
    #[serde(alias = "oldString")]
    old_string: String,
    #[serde(alias = "newString")]
    new_string: String,
    #[serde(default, alias = "replaceAll")]
    replace_all: bool,
}

impl HarnessTool for EditTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "edit",
            "Replace an exact string in a file. The target must be unique unless replace_all is true.",
            json!({
                "type": "object",
                "properties": {
                    "file_path": { "type": "string" },
                    "old_string": { "type": "string" },
                    "new_string": { "type": "string" },
                    "replace_all": { "type": "boolean", "default": false }
                },
                "required": ["file_path", "old_string", "new_string"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "edit",
            input_path::<EditInput>(input, |value| &value.file_path),
            "Edit a file",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: EditInput = parse(input)?;
        context.check_cancelled()?;
        if input.old_string.is_empty() {
            return Err(ToolError::new("old_string cannot be empty"));
        }
        let path = resolve(&self.cwd, &input.file_path);
        let old = fs::read_to_string(&path).map_err(tool_error)?;
        let matches = old.matches(&input.old_string).count();
        if matches == 0 {
            return Err(ToolError::new("old_string was not found in the file"));
        }
        if matches > 1 && !input.replace_all {
            return Err(ToolError::new(format!(
                "old_string matched {matches} places; provide more context or set replace_all"
            )));
        }
        let new = if input.replace_all {
            old.replace(&input.old_string, &input.new_string)
        } else {
            old.replacen(&input.old_string, &input.new_string, 1)
        };
        fs::write(&path, &new).map_err(tool_error)?;
        Ok(ToolOutput {
            title: display_path(&self.cwd, &path),
            output: "Edit applied successfully.".into(),
            diffs: vec![file_diff(&path, &old, &new)],
        })
    }
}

#[derive(Clone)]
struct ApplyPatchTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct PatchInput {
    patch: String,
}

impl HarnessTool for ApplyPatchTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "apply_patch",
            "Apply a unified diff to files in the working tree.",
            json!({
                "type": "object",
                "properties": { "patch": { "type": "string", "description": "Unified diff" } },
                "required": ["patch"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission("edit", patch_paths(input).join(","), "Apply a file patch")
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: PatchInput = parse(input)?;
        context.check_cancelled()?;
        let paths = unified_diff_paths(&input.patch);
        let before: BTreeMap<PathBuf, String> = paths
            .iter()
            .map(|path| {
                let full = resolve(&self.cwd, path);
                (full.clone(), fs::read_to_string(full).unwrap_or_default())
            })
            .collect();
        let mut child = Command::new("git")
            .args(["apply", "--whitespace=nowarn", "-"])
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(tool_error)?;
        child
            .stdin
            .as_mut()
            .ok_or_else(|| ToolError::new("Could not open git apply input"))?
            .write_all(input.patch.as_bytes())
            .map_err(tool_error)?;
        let output = child.wait_with_output().map_err(tool_error)?;
        if !output.status.success() {
            return Err(command_error("git apply", &output.stderr));
        }
        let diffs = before
            .into_iter()
            .map(|(path, old)| {
                let new = fs::read_to_string(&path).unwrap_or_default();
                file_diff(&path, &old, &new)
            })
            .collect();
        Ok(ToolOutput {
            title: format!("Patched {} file(s)", paths.len()),
            output: "Patch applied successfully.".into(),
            diffs,
        })
    }
}

#[derive(Clone)]
struct WebFetchTool {
    client: Client,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self {
            client: web_client(),
        }
    }
}

#[derive(Deserialize)]
struct WebFetchInput {
    url: String,
    #[serde(default = "markdown_format")]
    format: String,
    #[serde(default = "default_web_timeout")]
    timeout: u64,
}

impl HarnessTool for WebFetchTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "web_fetch",
            "Fetch an HTTP or HTTPS page and return bounded text, Markdown-like text, or raw HTML.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "format": { "type": "string", "enum": ["text", "markdown", "html"], "default": "markdown" },
                    "timeout": { "type": "integer", "minimum": 1, "maximum": 120 }
                },
                "required": ["url"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission("web_fetch", input_value(input, "url"), "Fetch a web page")
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: WebFetchInput = parse(input)?;
        context.check_cancelled()?;
        if !input.url.starts_with("https://") && !input.url.starts_with("http://") {
            return Err(ToolError::new("URL must start with http:// or https://"));
        }
        let response = self
            .client
            .get(&input.url)
            .timeout(Duration::from_secs(input.timeout.clamp(1, 120)))
            .send()
            .map_err(tool_error)?
            .error_for_status()
            .map_err(tool_error)?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_WEB_BYTES as u64)
        {
            return Err(ToolError::new("Response exceeds the 5 MB limit"));
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let mut bytes = Vec::new();
        response
            .take((MAX_WEB_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(tool_error)?;
        if bytes.len() > MAX_WEB_BYTES {
            return Err(ToolError::new("Response exceeds the 5 MB limit"));
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        let output = if input.format == "html" || !content_type.contains("html") {
            body
        } else {
            html2text::from_read(body.as_bytes(), 100).map_err(tool_error)?
        };
        Ok(ToolOutput {
            title: input.url,
            output: truncate_output(output),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct WebSearchTool {
    client: Client,
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self {
            client: web_client(),
        }
    }
}

#[derive(Deserialize)]
struct WebSearchInput {
    query: String,
    #[serde(default = "default_search_results", alias = "numResults")]
    num_results: usize,
    #[serde(default, alias = "livecrawl")]
    live_crawl: Option<String>,
    #[serde(default, rename = "type")]
    search_type: Option<String>,
}

impl HarnessTool for WebSearchTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "web_search",
            "Search the current web and return source titles, URLs, and relevant excerpts.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "num_results": { "type": "integer", "minimum": 1, "maximum": 10, "default": 8 },
                    "live_crawl": { "type": "string", "enum": ["fallback", "preferred"] },
                    "type": { "type": "string", "enum": ["auto", "fast", "deep"] }
                },
                "required": ["query"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission("web_search", input_value(input, "query"), "Search the web")
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: WebSearchInput = parse(input)?;
        context.check_cancelled()?;
        let mut url = "https://mcp.exa.ai/mcp".to_string();
        if let Ok(key) = std::env::var("EXA_API_KEY")
            && !key.is_empty()
        {
            url.push_str(&format!("?exaApiKey={}", urlencoding::encode(&key)));
        }
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "web_search_exa",
                "arguments": {
                    "query": input.query,
                    "type": input.search_type.unwrap_or_else(|| "auto".into()),
                    "numResults": input.num_results.clamp(1, 10),
                    "livecrawl": input.live_crawl.unwrap_or_else(|| "fallback".into())
                }
            }
        });
        let response = self
            .client
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .json(&request)
            .send()
            .map_err(tool_error)?
            .error_for_status()
            .map_err(tool_error)?
            .text()
            .map_err(tool_error)?;
        let output = parse_mcp_text(&response).unwrap_or_else(|| "No search results found.".into());
        Ok(ToolOutput {
            title: format!("Web Search: {}", input.query),
            output: truncate_output(output),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct RepoCloneTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct RepoCloneInput {
    url: String,
    #[serde(default)]
    directory: Option<String>,
    #[serde(default)]
    depth: Option<u32>,
}

impl HarnessTool for RepoCloneTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "repo_clone",
            "Clone a Git repository into the working directory or a specified destination.",
            json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string" },
                    "directory": { "type": "string" },
                    "depth": { "type": "integer", "minimum": 1 }
                },
                "required": ["url"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "repo_clone",
            input_value(input, "url"),
            "Clone a repository",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: RepoCloneInput = parse(input)?;
        context.check_cancelled()?;
        let mut command = Command::new("git");
        command.arg("clone");
        if let Some(depth) = input.depth {
            command.args(["--depth", &depth.to_string()]);
        }
        command.arg(&input.url);
        if let Some(directory) = &input.directory {
            command.arg(directory);
        }
        let output = command
            .current_dir(&self.cwd)
            .output()
            .map_err(tool_error)?;
        if !output.status.success() {
            return Err(command_error("git clone", &output.stderr));
        }
        Ok(ToolOutput {
            title: input.directory.unwrap_or(input.url),
            output: truncate_output(String::from_utf8_lossy(&output.stderr).into_owned()),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct RepoOverviewTool {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct RepoOverviewInput {
    #[serde(default)]
    path: Option<String>,
}

impl HarnessTool for RepoOverviewTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "repo_overview",
            "Inspect repository status, recent history, remotes, and top-level files.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "read",
            input_value(input, "path"),
            "Inspect repository metadata",
        )
    }

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: RepoOverviewInput = parse(input)?;
        context.check_cancelled()?;
        let root = resolve(&self.cwd, input.path.as_deref().unwrap_or("."));
        let command = "git status --short --branch; git log -5 --oneline --decorate; git remote -v; printf '\\nFiles:\\n'; find . -maxdepth 2 -not -path './.git*' -print | sort | head -200";
        let output = Command::new("bash")
            .args(["-lc", command])
            .current_dir(&root)
            .output()
            .map_err(tool_error)?;
        if !output.status.success() {
            return Err(command_error("repository overview", &output.stderr));
        }
        Ok(ToolOutput {
            title: display_path(&self.cwd, &root),
            output: truncate_output(String::from_utf8_lossy(&output.stdout).into_owned()),
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone, Default)]
struct TodoTool {
    items: Arc<Mutex<Vec<TodoItem>>>,
}

#[derive(Clone, Deserialize)]
struct TodoItem {
    content: String,
    status: String,
}

#[derive(Deserialize)]
struct TodoInput {
    todos: Vec<TodoItem>,
}

impl HarnessTool for TodoTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "todo_write",
            "Replace the current execution checklist. Use pending, in_progress, completed, or cancelled statuses.",
            json!({
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": { "type": "string" },
                                "status": { "type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"] }
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            }),
        )
    }

    fn permission(&self, _input: &str) -> ToolPermission {
        permission("todo", "session", "Update the execution checklist")
    }

    fn execute(&self, input: &str, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: TodoInput = parse(input)?;
        if input
            .todos
            .iter()
            .filter(|item| item.status == "in_progress")
            .count()
            > 1
        {
            return Err(ToolError::new("Only one todo can be in_progress"));
        }
        for item in &input.todos {
            if item.content.trim().is_empty() {
                return Err(ToolError::new("Todo content cannot be empty"));
            }
        }
        *self
            .items
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = input.todos.clone();
        let output = input
            .todos
            .into_iter()
            .map(|item| format!("[{}] {}", item.status, item.content))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput {
            title: "Updated execution checklist".into(),
            output,
            diffs: Vec::new(),
        })
    }
}

#[derive(Clone)]
struct JobTool {
    jobs: JobService,
}

#[derive(Deserialize)]
struct JobInput {
    action: String,
    #[serde(default)]
    job_id: Option<String>,
    #[serde(default)]
    goal: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule_type: Option<String>,
    #[serde(default)]
    interval_ms: Option<u64>,
    #[serde(default)]
    clock_times: Vec<String>,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    cron_expr: Option<String>,
}

impl HarnessTool for JobTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            "job",
            "Create, list, inspect, pause, resume, or complete persistent scheduled Jobs.",
            json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["create", "list", "get", "pause", "resume", "complete"] },
                    "job_id": { "type": "string" },
                    "goal": { "type": "string", "description": "Complete standalone instructions for a new Job" },
                    "name": { "type": "string", "description": "Short descriptive Job name" },
                    "schedule_type": { "type": "string", "enum": ["interval", "clock_based", "cron", "24_7"] },
                    "interval_ms": { "type": "integer", "minimum": 1000 },
                    "clock_times": { "type": "array", "items": { "type": "string", "description": "HH:mm" } },
                    "time_zone": { "type": "string", "description": "IANA time zone" },
                    "cron_expr": { "type": "string" }
                },
                "required": ["action"]
            }),
        )
    }

    fn permission(&self, input: &str) -> ToolPermission {
        permission(
            "job",
            input_value(input, "action"),
            "Manage persistent Jobs",
        )
    }

    fn execute(&self, input: &str, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let input: JobInput = parse(input)?;
        let job = match input.action.as_str() {
            "create" => {
                let goal = input
                    .goal
                    .filter(|goal| !goal.trim().is_empty())
                    .ok_or_else(|| ToolError::new("goal is required for create"))?;
                let name = input
                    .name
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| {
                        goal.split_whitespace()
                            .take(8)
                            .collect::<Vec<_>>()
                            .join(" ")
                    });
                let schedule = match input.schedule_type.as_deref() {
                    Some("interval") => JobSchedule::Interval {
                        interval_ms: input.interval_ms.unwrap_or(60_000).max(1_000),
                    },
                    Some("clock_based") => {
                        if input.clock_times.is_empty() {
                            return Err(ToolError::new(
                                "clock_times is required for a clock_based schedule",
                            ));
                        }
                        JobSchedule::ClockBased {
                            clock_times: input.clock_times,
                            time_zone: input.time_zone,
                        }
                    }
                    Some("cron") => JobSchedule::Cron {
                        cron_expr: input
                            .cron_expr
                            .filter(|value| !value.trim().is_empty())
                            .ok_or_else(|| ToolError::new("cron_expr is required for cron"))?,
                    },
                    Some("24_7") | None => JobSchedule::Continuous,
                    Some(value) => {
                        return Err(ToolError::new(format!(
                            "Unknown Job schedule type: {value}"
                        )));
                    }
                };
                Some(self.jobs.create(goal, name, schedule).map_err(tool_error)?)
            }
            "list" => {
                let jobs = self.jobs.list();
                let output = if jobs.is_empty() {
                    "No Jobs configured.".into()
                } else {
                    jobs.into_iter()
                        .map(|job| {
                            format!(
                                "{}  {:?}  {}  {}",
                                job.id,
                                job.status,
                                job.name,
                                job.schedule_description()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                return Ok(ToolOutput {
                    title: "Persistent Jobs".into(),
                    output,
                    diffs: Vec::new(),
                });
            }
            action => {
                let id = input
                    .job_id
                    .as_deref()
                    .ok_or_else(|| ToolError::new(format!("job_id is required for {action}")))?;
                match action {
                    "get" => self.jobs.get(id),
                    "pause" => self.jobs.pause(id).map_err(tool_error)?,
                    "resume" => self.jobs.resume(id).map_err(tool_error)?,
                    "complete" => self.jobs.complete(id).map_err(tool_error)?,
                    _ => return Err(ToolError::new(format!("Unknown Job action: {action}"))),
                }
            }
        };
        let job = job.ok_or_else(|| ToolError::new("Job not found"))?;
        Ok(ToolOutput {
            title: job.name.clone(),
            output: serde_json::to_string_pretty(&job).map_err(tool_error)?,
            diffs: Vec::new(),
        })
    }
}

fn definition(name: &str, description: &str, schema: Value) -> ToolDefinition {
    ToolDefinition {
        name: name.into(),
        description: description.into(),
        input_schema: schema.to_string(),
    }
}

fn permission(
    permission_name: &str,
    pattern: impl Into<String>,
    description: &str,
) -> ToolPermission {
    ToolPermission {
        permission: permission_name.into(),
        patterns: vec![pattern.into()],
        description: description.into(),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(input: &str) -> Result<T, ToolError> {
    serde_json::from_str(input)
        .map_err(|error| ToolError::new(format!("Invalid tool input: {error}")))
}

fn input_value(input: &str, key: &str) -> String {
    serde_json::from_str::<Value>(input)
        .ok()
        .and_then(|value| value.get(key).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "*".into())
}

fn input_path<T: for<'de> Deserialize<'de>>(input: &str, get: impl FnOnce(&T) -> &str) -> String {
    parse::<T>(input)
        .ok()
        .map(|value| get(&value).to_owned())
        .unwrap_or_else(|| "*".into())
}

fn resolve(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn display_path(cwd: &Path, path: &Path) -> String {
    path.strip_prefix(cwd).unwrap_or(path).display().to_string()
}

fn file_diff(path: &Path, old: &str, new: &str) -> FileDiff {
    let diff = TextDiff::from_lines(old, new);
    let mut old_line = 1usize;
    let mut new_line = 1usize;
    let mut lines = Vec::new();
    for change in diff.iter_all_changes() {
        let text = change.value().trim_end_matches(['\r', '\n']).to_string();
        match change.tag() {
            ChangeTag::Equal => {
                lines.push(DiffLine {
                    old_line: Some(old_line),
                    new_line: Some(new_line),
                    kind: DiffKind::Context,
                    text,
                });
                old_line += 1;
                new_line += 1;
            }
            ChangeTag::Delete => {
                lines.push(DiffLine {
                    old_line: Some(old_line),
                    new_line: None,
                    kind: DiffKind::Removed,
                    text,
                });
                old_line += 1;
            }
            ChangeTag::Insert => {
                lines.push(DiffLine {
                    old_line: None,
                    new_line: Some(new_line),
                    kind: DiffKind::Added,
                    text,
                });
                new_line += 1;
            }
        }
    }
    FileDiff {
        path: path.display().to_string(),
        lines,
    }
}

fn unified_diff_paths(patch: &str) -> Vec<String> {
    let mut output = Vec::new();
    for line in patch.lines().filter_map(|line| line.strip_prefix("+++ ")) {
        let path = line
            .split_whitespace()
            .next()
            .unwrap_or(line)
            .trim_start_matches("b/");
        if path != "/dev/null" && !output.iter().any(|item| item == path) {
            output.push(path.into());
        }
    }
    output
}

fn patch_paths(input: &str) -> Vec<String> {
    parse::<PatchInput>(input)
        .map(|input| unified_diff_paths(&input.patch))
        .unwrap_or_else(|_| vec!["*".into()])
}

fn binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(4_096)];
    if sample.contains(&0) {
        return true;
    }
    let non_printable = sample
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    !sample.is_empty() && non_printable * 10 > sample.len() * 3
}

fn truncate_line(line: &str) -> String {
    if line.len() <= MAX_LINE_BYTES {
        line.to_string()
    } else {
        let mut end = MAX_LINE_BYTES;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &line[..end])
    }
}

fn truncate_output(output: impl Into<String>) -> String {
    let output = output.into();
    if output.len() <= MAX_TOOL_OUTPUT {
        return output;
    }
    let mut start = output.len() - MAX_TOOL_OUTPUT;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    format!("(Earlier output truncated.)\n{}", &output[start..])
}

fn command_title(command: &str) -> String {
    let line = command.lines().next().unwrap_or(command).trim();
    if line.chars().count() <= 80 {
        line.into()
    } else {
        format!("{}…", line.chars().take(79).collect::<String>())
    }
}

fn command_error(command: &str, stderr: &[u8]) -> ToolError {
    let detail = String::from_utf8_lossy(stderr);
    ToolError::new(format!("{command} failed: {}", detail.trim()))
}

fn tool_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::new(error.to_string())
}

fn parse_mcp_text(body: &str) -> Option<String> {
    fn parse_value(value: &str) -> Option<String> {
        serde_json::from_str::<Value>(value)
            .ok()?
            .pointer("/result/content")
            .and_then(Value::as_array)?
            .iter()
            .find_map(|item| item.get("text").and_then(Value::as_str).map(str::to_owned))
    }
    parse_value(body.trim()).or_else(|| {
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .find_map(|line| parse_value(line.trim()))
    })
}

fn web_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("indus/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("web tool HTTP client should initialize")
}

fn one() -> usize {
    1
}
fn default_read_limit() -> usize {
    MAX_READ_LINES
}
fn markdown_format() -> String {
    "markdown".into()
}
fn default_web_timeout() -> u64 {
    30
}
fn default_search_results() -> usize {
    8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_diff_tracks_line_numbers() {
        let diff = file_diff(Path::new("a.txt"), "one\ntwo\n", "one\nthree\n");
        assert!(
            diff.lines
                .iter()
                .any(|line| line.kind == DiffKind::Removed && line.old_line == Some(2))
        );
        assert!(
            diff.lines
                .iter()
                .any(|line| line.kind == DiffKind::Added && line.new_line == Some(2))
        );
    }

    #[test]
    fn mcp_search_response_extracts_text_content() {
        let body = r#"{"result":{"content":[{"type":"text","text":"result"}]}}"#;
        assert_eq!(parse_mcp_text(body).as_deref(), Some("result"));
    }

    #[test]
    fn patch_paths_ignore_deletions() {
        let patch = "--- a/old\n+++ /dev/null\n--- /dev/null\n+++ b/new.txt\n";
        assert_eq!(unified_diff_paths(patch), vec!["new.txt"]);
    }
}
