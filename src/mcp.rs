use crate::db::{Db, Priority, Status};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

#[derive(Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

fn success(id: Value, result: Value) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: Some(result),
        error: None,
    }
}

fn error(id: Value, code: i64, message: String) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id,
        result: None,
        error: Some(JsonRpcError { code, message }),
    }
}

fn tool_result(text: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }]
    })
}

fn tool_error(text: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

fn handle_initialize(id: Value) -> JsonRpcResponse {
    success(
        id,
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "cli-todo", "version": "0.1.0" }
        }),
    )
}

fn tool_schema() -> Value {
    json!({
        "tools": [
            {
                "name": "list_tasks",
                "description": "List all tasks. Returns task tree with id, title, status, priority, tags, parent_id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "enum": ["todo", "in_progress", "in_review", "done", "blocked"],
                            "description": "Filter by status. Lifecycle: todo → in_progress → in_review (completed in worktree) → done (merged to main)"
                        },
                        "parent_id": {
                            "type": "integer",
                            "description": "Filter to direct children of this task ID"
                        }
                    }
                }
            },
            {
                "name": "get_task",
                "description": "Get a single task by ID with full details including descendant count and session count.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "integer", "description": "The task ID" }
                    },
                    "required": ["task_id"]
                }
            },
            {
                "name": "add_task",
                "description": "Create a new task. Returns the new task ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string", "description": "Task title" },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "critical"],
                            "description": "Priority level. Defaults to medium."
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Tags for the task"
                        },
                        "description": { "type": "string", "description": "Detailed description" },
                        "parent_id": { "type": "integer", "description": "Parent task ID for nesting" }
                    },
                    "required": ["title"]
                }
            },
            {
                "name": "update_task",
                "description": "Update one or more fields of an existing task.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "integer", "description": "The task ID to update" },
                        "title": { "type": "string", "description": "New title" },
                        "status": {
                            "type": "string",
                            "enum": ["todo", "in_progress", "in_review", "done", "blocked"],
                            "description": "New status. Use in_review when changes are ready for review in a worktree, done when merged to main"
                        },
                        "priority": {
                            "type": "string",
                            "enum": ["low", "medium", "high", "critical"],
                            "description": "New priority"
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "New tags (replaces existing)"
                        },
                        "description": { "type": "string", "description": "New description" },
                        "parent_id": {
                            "type": ["integer", "null"],
                            "description": "New parent task ID, or null to make root-level"
                        }
                    },
                    "required": ["task_id"]
                }
            },
            {
                "name": "delete_task",
                "description": "Delete a task by ID. Also deletes all subtasks (cascade).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "task_id": { "type": "integer", "description": "The task ID to delete" }
                    },
                    "required": ["task_id"]
                }
            }
        ]
    })
}

fn handle_tools_call(id: Value, params: &Value, db: &Db) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "list_tasks" => call_list_tasks(&args, db),
        "get_task" => call_get_task(&args, db),
        "add_task" => call_add_task(&args, db),
        "update_task" => call_update_task(&args, db),
        "delete_task" => call_delete_task(&args, db),
        _ => tool_error(format!("Unknown tool: {tool_name}")),
    };

    success(id, result)
}

fn call_list_tasks(args: &Value, db: &Db) -> Value {
    let tasks = match db.all_tasks() {
        Ok(t) => t,
        Err(e) => return tool_error(format!("DB error: {e}")),
    };

    let status_filter = args
        .get("status")
        .and_then(|v| v.as_str())
        .map(Status::from_str);
    let parent_filter = args.get("parent_id").and_then(|v| v.as_i64());

    let filtered: Vec<_> = tasks
        .into_iter()
        .filter(|t| status_filter.is_none_or(|s| t.status == s))
        .filter(|t| {
            if let Some(pid) = parent_filter {
                t.parent_id == Some(pid)
            } else {
                true
            }
        })
        .collect();

    let json_tasks: Vec<Value> = filtered.iter().map(|t| json!(t)).collect();
    tool_result(serde_json::to_string_pretty(&json_tasks).unwrap())
}

fn call_get_task(args: &Value, db: &Db) -> Value {
    let task_id = match args.get("task_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return tool_error("task_id is required".into()),
    };

    let task = match db.get_task(task_id) {
        Ok(Some(t)) => t,
        Ok(None) => return tool_error(format!("Task {task_id} not found")),
        Err(e) => return tool_error(format!("DB error: {e}")),
    };

    let descendant_count = db.descendant_count(task_id).unwrap_or(0);
    let session_count = db.session_count(task_id).unwrap_or(0);

    let mut task_json = json!(task);
    task_json["descendant_count"] = json!(descendant_count);
    task_json["session_count"] = json!(session_count);

    tool_result(serde_json::to_string_pretty(&task_json).unwrap())
}

fn call_add_task(args: &Value, db: &Db) -> Value {
    let title = match args.get("title").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return tool_error("title is required".into()),
    };

    let priority = args
        .get("priority")
        .and_then(|v| v.as_str())
        .map(Priority::from_str)
        .unwrap_or(Priority::Medium);

    let tags: Vec<String> = args
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let parent_id = args.get("parent_id").and_then(|v| v.as_i64());

    match db.add_task(title, priority, &tags, description, parent_id) {
        Ok(id) => tool_result(json!({"task_id": id}).to_string()),
        Err(e) => tool_error(format!("DB error: {e}")),
    }
}

fn call_update_task(args: &Value, db: &Db) -> Value {
    let task_id = match args.get("task_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return tool_error("task_id is required".into()),
    };

    let title = args.get("title").and_then(|v| v.as_str());
    let status = args
        .get("status")
        .and_then(|v| v.as_str())
        .map(Status::from_str);
    let priority = args
        .get("priority")
        .and_then(|v| v.as_str())
        .map(Priority::from_str);
    let tags: Option<Vec<String>> = args.get("tags").and_then(|v| v.as_array()).map(|arr| {
        arr.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    });
    let description = args.get("description").and_then(|v| v.as_str());

    match db.update_task(
        task_id,
        title,
        status,
        priority,
        tags.as_deref(),
        description,
    ) {
        Ok(true) => {
            // Handle parent_id reparenting if provided
            if let Some(parent_val) = args.get("parent_id") {
                let new_parent = if parent_val.is_null() {
                    None
                } else {
                    parent_val.as_i64()
                };
                if let Err(e) = db.reparent_task(task_id, new_parent) {
                    return tool_error(format!("Reparent failed: {e}"));
                }
            }
            tool_result(json!({"updated": true}).to_string())
        }
        Ok(false) => tool_error(format!("Task {task_id} not found")),
        Err(e) => tool_error(format!("DB error: {e}")),
    }
}

fn call_delete_task(args: &Value, db: &Db) -> Value {
    let task_id = match args.get("task_id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return tool_error("task_id is required".into()),
    };

    match db.delete_task(task_id) {
        Ok(()) => tool_result(json!({"deleted": true}).to_string()),
        Err(e) => tool_error(format!("DB error: {e}")),
    }
}

pub fn run() {
    let db = Db::open().expect("failed to open database");
    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    for line in stdin.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Parse error: {e}");
                let resp = error(Value::Null, -32700, format!("Parse error: {e}"));
                let _ = serde_json::to_writer(&mut stdout, &resp);
                let _ = writeln!(stdout);
                let _ = stdout.flush();
                continue;
            }
        };

        // Notifications have no id — no response needed
        let id = match request.id {
            Some(id) => id,
            None => {
                eprintln!("Notification: {}", request.method);
                continue;
            }
        };

        let params = request.params.unwrap_or(json!({}));

        let response = match request.method.as_str() {
            "initialize" => handle_initialize(id),
            "tools/list" => success(id, tool_schema()),
            "tools/call" => handle_tools_call(id, &params, &db),
            _ => error(id, -32601, format!("Method not found: {}", request.method)),
        };

        let _ = serde_json::to_writer(&mut stdout, &response);
        let _ = writeln!(stdout);
        let _ = stdout.flush();
    }
}
