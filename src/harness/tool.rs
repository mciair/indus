//! Tool registry and execution contract for the Indus harness.

use std::{
    collections::BTreeMap,
    error::Error,
    fmt,
    sync::{Arc, RwLock},
};

use super::{
    event::FileDiff,
    model::{CancellationToken, ToolDefinition},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolPermission {
    pub permission: String,
    pub patterns: Vec<String>,
    pub description: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolOutput {
    pub title: String,
    pub output: String,
    pub diffs: Vec<FileDiff>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolError {
    pub message: String,
}

impl ToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ToolError {}

#[derive(Clone)]
pub struct ToolContext {
    pub run_id: u64,
    pub call_id: String,
    pub cancellation: CancellationToken,
    output: Arc<dyn Fn(String) + Send + Sync>,
}

impl fmt::Debug for ToolContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolContext")
            .field("run_id", &self.run_id)
            .field("call_id", &self.call_id)
            .field("cancelled", &self.cancellation.is_cancelled())
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    pub fn new(
        run_id: u64,
        call_id: impl Into<String>,
        cancellation: CancellationToken,
        output: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Self {
            run_id,
            call_id: call_id.into(),
            cancellation,
            output: Arc::new(output),
        }
    }

    pub fn emit_output(&self, chunk: impl Into<String>) {
        (self.output)(chunk.into());
    }

    pub fn check_cancelled(&self) -> Result<(), ToolError> {
        if self.cancellation.is_cancelled() {
            Err(ToolError::new("Tool execution cancelled"))
        } else {
            Ok(())
        }
    }
}

pub trait HarnessTool: Send + Sync + 'static {
    fn definition(&self) -> ToolDefinition;

    fn permission(&self, input: &str) -> ToolPermission;

    fn execute(&self, input: &str, context: &ToolContext) -> Result<ToolOutput, ToolError>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<RwLock<BTreeMap<String, Arc<dyn HarnessTool>>>>,
}

impl ToolRegistry {
    pub fn register(&self, tool: impl HarnessTool) -> Option<Arc<dyn HarnessTool>> {
        let tool: Arc<dyn HarnessTool> = Arc::new(tool);
        let name = tool.definition().name;
        self.tools
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name, tool)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn HarnessTool>> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(name)
            .cloned()
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|tool| tool.definition())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ExampleTool;

    impl HarnessTool for ExampleTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "example".into(),
                description: "Example tool".into(),
                input_schema: "{}".into(),
            }
        }

        fn permission(&self, _input: &str) -> ToolPermission {
            ToolPermission {
                permission: "example".into(),
                patterns: vec!["*".into()],
                description: "Run example".into(),
            }
        }

        fn execute(&self, input: &str, _context: &ToolContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput {
                title: "Example complete".into(),
                output: input.to_string(),
                diffs: Vec::new(),
            })
        }
    }

    #[test]
    fn registry_exposes_registered_tool_definitions() {
        let registry = ToolRegistry::default();
        assert!(registry.is_empty());
        registry.register(ExampleTool);
        assert_eq!(registry.definitions()[0].name, "example");
        assert!(registry.get("example").is_some());
    }
}
