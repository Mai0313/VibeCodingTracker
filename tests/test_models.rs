// Tests for models module

use vibe_coding_tracker::models::*;

#[test]
fn test_extension_type_display() {
    assert_eq!(ExtensionType::ClaudeCode.to_string(), "Claude-Code");
    assert_eq!(ExtensionType::Codex.to_string(), "Codex");
}

#[test]
fn test_extension_type_equality() {
    assert_eq!(ExtensionType::ClaudeCode, ExtensionType::ClaudeCode);
    assert_eq!(ExtensionType::Codex, ExtensionType::Codex);
    assert_ne!(ExtensionType::ClaudeCode, ExtensionType::Codex);
}

#[test]
fn test_code_analysis_tool_calls_default() {
    let tool_calls = CodeAnalysisToolCalls::default();
    assert_eq!(tool_calls.read, 0);
    assert_eq!(tool_calls.write, 0);
    assert_eq!(tool_calls.edit, 0);
    assert_eq!(tool_calls.todo_write, 0);
    assert_eq!(tool_calls.bash, 0);
}

#[test]
fn test_code_analysis_tool_calls_serialization() {
    let tool_calls = CodeAnalysisToolCalls {
        read: 5,
        write: 3,
        edit: 2,
        todo_write: 1,
        bash: 4,
    };

    let json = serde_json::to_value(&tool_calls).unwrap();
    assert_eq!(json["Read"], 5);
    assert_eq!(json["Write"], 3);
    assert_eq!(json["Edit"], 2);
    assert_eq!(json["TodoWrite"], 1);
    assert_eq!(json["Bash"], 4);
}

#[test]
fn test_code_analysis_detail_base_serialization() {
    let detail = CodeAnalysisDetailBase {
        file_path: "/path/to/file.rs".to_string(),
        line_count: 100,
        character_count: 2500,
        timestamp: 1724851028611,
    };

    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["filePath"], "/path/to/file.rs");
    assert_eq!(json["lineCount"], 100);
    assert_eq!(json["characterCount"], 2500);
    assert_eq!(json["timestamp"], 1724851028611i64);
}

#[test]
fn test_code_analysis_write_detail_serialization() {
    let detail = CodeAnalysisWriteDetail {
        base: CodeAnalysisDetailBase {
            file_path: "/path/to/file.rs".to_string(),
            line_count: 10,
            character_count: 250,
            timestamp: 1724851028611,
        },
        content: "fn main() {}".to_string(),
    };

    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["filePath"], "/path/to/file.rs");
    assert_eq!(json["content"], "fn main() {}");
}

#[test]
fn test_code_analysis_read_detail_serialization() {
    let detail = CodeAnalysisReadDetail {
        base: CodeAnalysisDetailBase {
            file_path: "/path/to/file.rs".to_string(),
            line_count: 50,
            character_count: 1500,
            timestamp: 1724851028611,
        },
    };

    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["filePath"], "/path/to/file.rs");
    assert_eq!(json["lineCount"], 50);
}

#[test]
fn test_code_analysis_apply_diff_detail_serialization() {
    let detail = CodeAnalysisApplyDiffDetail {
        base: CodeAnalysisDetailBase {
            file_path: "/path/to/file.rs".to_string(),
            line_count: 5,
            character_count: 150,
            timestamp: 1724851028611,
        },
        old_string: "old code".to_string(),
        new_string: "new code".to_string(),
    };

    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["filePath"], "/path/to/file.rs");
    assert_eq!(json["oldString"], "old code");
    assert_eq!(json["newString"], "new code");
}

#[test]
fn test_code_analysis_run_command_detail_serialization() {
    let detail = CodeAnalysisRunCommandDetail {
        base: CodeAnalysisDetailBase {
            file_path: "/working/dir".to_string(),
            line_count: 0,
            character_count: 15,
            timestamp: 1724851028611,
        },
        command: "cargo build".to_string(),
        description: "Build the project".to_string(),
    };

    let json = serde_json::to_value(&detail).unwrap();
    assert_eq!(json["command"], "cargo build");
    assert_eq!(json["description"], "Build the project");
}

#[test]
fn test_code_analysis_record_deserialization() {
    let json_str = r#"{
        "totalUniqueFiles": 5,
        "totalWriteLines": 100,
        "totalReadLines": 200,
        "totalEditLines": 50,
        "totalWriteCharacters": 2500,
        "totalReadCharacters": 5000,
        "totalEditCharacters": 1200,
        "writeFileDetails": [],
        "readFileDetails": [],
        "editFileDetails": [],
        "runCommandDetails": [],
        "toolCallCounts": {
            "Read": 10,
            "Write": 5,
            "Edit": 3,
            "TodoWrite": 2,
            "Bash": 4
        },
        "conversationUsage": {},
        "taskId": "test-task-id",
        "timestamp": 1724851028611,
        "folderPath": "/home/user/project",
        "gitRemoteUrl": "https://github.com/test/repo.git"
    }"#;

    let record: CodeAnalysisRecord = serde_json::from_str(json_str).unwrap();
    assert_eq!(record.total_unique_files, 5);
    assert_eq!(record.total_write_lines, 100);
    assert_eq!(record.tool_call_counts.read, 10);
    assert_eq!(record.task_id, "test-task-id");
}

#[test]
fn test_code_analysis_full_serialization() {
    let analysis = CodeAnalysis {
        user: "testuser".to_string(),
        extension_name: "Claude-Code".to_string(),
        insights_version: "1.0.0".to_string(),
        machine_id: "test-machine".to_string(),
        records: vec![],
    };

    let json = serde_json::to_value(&analysis).unwrap();
    assert_eq!(json["user"], "testuser");
    assert_eq!(json["extensionName"], "Claude-Code");
    assert_eq!(json["insightsVersion"], "1.0.0");
    assert_eq!(json["machineId"], "test-machine");
    assert!(json["records"].is_array());
}
