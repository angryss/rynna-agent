//! Shared native tool composition for CLI/server and desktop adapters.
use async_trait::async_trait;
use rynna_config::{ResolvedCapability, ResolvedProfile};
use rynna_core::{Tool, ToolDefinition, ToolError};
use rynna_tools_command::{CommandConfig, CommandTool};
use rynna_tools_filesystem::{FileSystemConfig, FileSystemToolset};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub fn native_tools(profile: &ResolvedProfile) -> Result<Vec<Arc<dyn Tool>>, String> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(HostInfo)];
    if profile.yolo {
        let mut config = FileSystemConfig::new(&profile.profile.default_project_directory);
        config.denied_patterns.clear();
        config.protected_patterns.clear();
        config.max_read_bytes = usize::MAX - 1;
        config.max_results = usize::MAX - 1;
        config.max_traversal_files = usize::MAX - 1;
        config.max_traversal_depth = usize::MAX - 1;
        config.max_search_bytes = usize::MAX - 1;
        for tool in FileSystemToolset::new(config.clone())
            .map_err(|e| e.to_string())?
            .tools()
        {
            tools.push(Arc::new(ProjectFileTool {
                definition: tool.definition(),
                config: config.clone(),
                directories: vec![config.root.clone()],
                yolo: true,
                cache: Mutex::new(BTreeMap::new()),
            }));
        }
        tools.push(Arc::new(ProjectCommandTool {
            directory: profile.profile.default_project_directory.clone(),
        }));
        return Ok(tools);
    }
    for capability in &profile.capabilities {
        match capability {
            ResolvedCapability::Command(capability) => {
                tools.push(Arc::new(
                    CommandTool::new(CommandConfig {
                        working_directory: capability.working_directory.clone(),
                        programs: capability.programs.clone(),
                        timeout_seconds: capability.timeout_seconds,
                        max_output_bytes: capability.max_output_bytes,
                    })
                    .map_err(|e| e.to_string())?,
                ));
            }
            ResolvedCapability::FileSystem(capability) => {
                let mut config = FileSystemConfig::new(&capability.root);
                config.read_only = capability.read_only;
                config.allowed_patterns = capability.allowed_patterns.clone();
                if let Some(patterns) = &capability.denied_patterns {
                    config.denied_patterns.clone_from(patterns);
                }
                if let Some(patterns) = &capability.protected_patterns {
                    config.protected_patterns.clone_from(patterns);
                }
                if let Some(limit) = capability.max_read_bytes {
                    config.max_read_bytes = limit;
                }
                if let Some(limit) = capability.max_results {
                    config.max_results = limit;
                }
                if let Some(limit) = capability.max_traversal_files {
                    config.max_traversal_files = limit;
                }
                if let Some(limit) = capability.max_traversal_depth {
                    config.max_traversal_depth = limit;
                }
                if let Some(limit) = capability.max_search_bytes {
                    config.max_search_bytes = limit;
                }
                tools.extend(
                    FileSystemToolset::new(config)
                        .map_err(|e| e.to_string())?
                        .tools(),
                );
            }
        }
    }

    if !profile
        .capabilities
        .iter()
        .any(|c| matches!(c, ResolvedCapability::FileSystem(_)))
    {
        let mut config = FileSystemConfig::new(&profile.profile.default_project_directory);
        config.read_only = true;
        for tool in FileSystemToolset::new(config.clone())
            .map_err(|e| e.to_string())?
            .tools()
        {
            if matches!(
                tool.definition().name.as_str(),
                "write_file" | "edit_file" | "create_directory"
            ) {
                continue;
            }
            tools.push(Arc::new(ProjectFileTool {
                definition: tool.definition(),
                config: config.clone(),
                directories: vec![config.root.clone()],
                yolo: false,
                cache: Mutex::new(BTreeMap::new()),
            }));
        }
    }
    Ok(tools)
}

struct ProjectFileTool {
    definition: ToolDefinition,
    config: FileSystemConfig,
    directories: Vec<PathBuf>,
    yolo: bool,
    cache: Mutex<BTreeMap<PathBuf, Arc<dyn Tool>>>,
}
#[async_trait]
impl Tool for ProjectFileTool {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }
    fn workflow_policy(&self) -> String {
        format!("default-project:{:?}:{:?}", self.config, self.directories)
    }
    fn for_project(&self, directories: &[PathBuf]) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            definition: self.definition.clone(),
            config: self.config.clone(),
            directories: directories.to_vec(),
            yolo: self.yolo,
            cache: Mutex::new(BTreeMap::new()),
        }))
    }
    async fn execute(&self, mut arguments: Value) -> Result<Value, ToolError> {
        let mut config = self.config.clone();
        config.root = self
            .directories
            .first()
            .cloned()
            .unwrap_or_else(|| config.root.clone());
        if self.yolo {
            // Resolve the requested path (including symlinks) using ambient authority.
            // The native adapter retains argument and optimistic-write validation.
            let path = match arguments.get("path") {
                Some(Value::String(path)) if !path.is_empty() => path.as_str(),
                None if self.definition.name == "code_search" => ".",
                _ => return Err(ToolError::new("path must be a non-empty string")),
            };
            let absolute = if Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                config.root.join(path)
            };
            let absolute = resolve_existing_ancestor(&absolute)?;
            if self.definition.name == "code_search" {
                config.root = absolute;
                arguments["path"] = json!(".");
            } else {
                config.root = absolute.ancestors().last().unwrap().to_owned();
                let relative = absolute
                    .strip_prefix(&config.root)
                    .unwrap()
                    .to_string_lossy();
                arguments["path"] = json!(if relative.is_empty() { "." } else { &relative });
            }
        } else if let Some(path) = arguments.get("path").and_then(Value::as_str)
            && Path::new(path).is_absolute()
        {
            let matching = self
                .directories
                .iter()
                .filter_map(|d| d.canonicalize().ok())
                .find(|d| Path::new(path).starts_with(d));
            let directory =
                matching.ok_or_else(|| ToolError::new("path is outside the selected project"))?;
            let relative = Path::new(path)
                .strip_prefix(&directory)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            config.root = directory;
            arguments["path"] = json!(if relative.is_empty() { "." } else { &relative });
        }
        let tool = {
            let mut cache = self
                .cache
                .lock()
                .map_err(|_| ToolError::new("project tool cache unavailable"))?;
            if let Some(tool) = cache.get(&config.root) {
                tool.clone()
            } else {
                let root = config.root.clone();
                let tool = FileSystemToolset::new(config)
                    .map_err(|e| ToolError::new(e.to_string()))?
                    .tools()
                    .into_iter()
                    .find(|t| t.definition().name == self.definition.name)
                    .unwrap();
                cache.insert(root, tool.clone());
                tool
            }
        };
        tool.execute(arguments).await
    }
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, ToolError> {
    if let Ok(canonical) = path.canonicalize() {
        return Ok(canonical);
    }
    let parent = path
        .parent()
        .ok_or_else(|| ToolError::new("cannot resolve path"))?;
    let name = path
        .file_name()
        .ok_or_else(|| ToolError::new("invalid path"))?;
    Ok(resolve_existing_ancestor(parent)?.join(name))
}
struct ProjectCommandTool {
    directory: PathBuf,
}
#[async_trait]
impl Tool for ProjectCommandTool {
    fn definition(&self) -> ToolDefinition {
        // Definition must not depend on whether the selected project still exists.
        ToolDefinition::new(
            "run_command",
            "YOLO: run any executable (PATH name or absolute path) in the selected project. Pass shell syntax via an explicit shell. Inherits process environment and OS permissions, no permission prompts, stdin closed.",
            json!({"type":"object","properties":{"program":{"type":"string"},"arguments":{"type":"array","items":{"type":"string"}}},"required":["program"],"additionalProperties":false}),
        )
    }
    fn workflow_policy(&self) -> String {
        format!("yolo-command:{:?}", self.directory)
    }
    fn for_project(&self, directories: &[PathBuf]) -> Option<Arc<dyn Tool>> {
        directories.first().map(|d| {
            Arc::new(Self {
                directory: d.clone(),
            }) as Arc<dyn Tool>
        })
    }
    async fn execute(&self, arguments: Value) -> Result<Value, ToolError> {
        CommandTool::new_yolo(self.directory.clone())
            .map_err(|e| ToolError::new(e.to_string()))?
            .execute(arguments)
            .await
    }
}

mod host_info;
use host_info::HostInfo;
