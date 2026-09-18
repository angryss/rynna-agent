//! Profile-scoped switches restrict tools; they never grant capabilities.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ToolsetId {
    FileOperations,
    CodeSearch,
    Commands,
    Skills,
    Subagents,
}

impl ToolsetId {
    pub fn contains(self, tool: &str) -> bool {
        match self {
            Self::FileOperations => matches!(
                tool,
                "read_file"
                    | "write_file"
                    | "edit_file"
                    | "search_files"
                    | "find_files"
                    | "list_directory"
                    | "create_directory"
                    | "file_info"
            ),
            Self::CodeSearch => tool == "code_search",
            Self::Commands => tool == "run_command",
            Self::Skills => tool == "read_skill",
            Self::Subagents => tool == "delegate_task",
        }
    }
}
