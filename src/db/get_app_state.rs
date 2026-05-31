use crate::db::update;
use axum::{Json, http::StatusCode, response::IntoResponse};
use nvml_wrapper::Nvml;
use redis::{Client, JsonCommands};
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;
// use std::io;

pub async fn get_db() -> impl IntoResponse {
    check_blend_file();
    blender_version();
    gpu_count();

    // Try to connect to Redis
    let client = match Client::open("redis://127.0.0.1:6379/") {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message" : "Redis connection error. Please try again"})),
            );
        }
    };

    let mut con = match client.get_connection() {
        Ok(c) => c,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message" : "Redis connection error. Please try again"})),
            );
        }
    };

    // Fetch and parse JSON
    let raw: String = match con.json_get("items", ".") {
        Ok(r) => r,
        Err(_) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"message" : "Key not found or Redis error"})),
            );
        }
    };

    let json_val: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message" : "JSON parse error"})),
            );
        }
    };

    (StatusCode::OK, Json(json_val))
}

fn check_blend_file() -> Option<impl IntoResponse> {
    let base_path = Path::new("./blend-folder");
    let mut blend_file_exist: bool = false;

    // Recursively find .blend files under blend-folder (supports zip-extracted subdirectories)
    let mut blend_files: Vec<String> = Vec::new();
    collect_blend_files_recursive(base_path, base_path, &mut blend_files);

    // Sort alphabetically for deterministic selection
    blend_files.sort();

    // Get first blend file name (if any)
    let first_blend_file = blend_files.get(0).cloned();

    if let Some(ref name) = first_blend_file {
        if !name.is_empty() {
            blend_file_exist = true;
        }
    }

    // Store in database
    let db_field = json!({
        "blend_file": {
            "is_present": blend_file_exist,
            "file_name": first_blend_file
        }
    });

    if let Err(_) = update(db_field) {
        return Some((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "Failed to update database" })),
        ));
    }

    None
}

/// Recursively collect .blend file paths relative to base_dir.
fn collect_blend_files_recursive(base_dir: &Path, current_dir: &Path, results: &mut Vec<String>) {
    let entries = match fs::read_dir(current_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            // Skip temp_chunks directory
            if path.file_name().and_then(|n| n.to_str()) == Some("temp_chunks") {
                continue;
            }
            collect_blend_files_recursive(base_dir, &path, results);
        } else if file_type.is_file() {
            if path.extension()
                .and_then(|ext| ext.to_str())
                .map(|e| e.eq_ignore_ascii_case("blend"))
                == Some(true)
            {
                if let Ok(relative) = path.strip_prefix(base_dir) {
                    let relative_str = relative.to_string_lossy().replace("\\", "/");
                    results.push(relative_str);
                }
            }
        }
    }
}

fn blender_version() {
    let output = Command::new("./blender/blender")
        .arg("--version")
        .output()
        .expect("Failed to execute blender");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(first_line) = stdout.lines().next() {
            let data = json!({"blender_version" : first_line});

            update(data).unwrap();
        } else {
            panic!("No output from blender");
        }
    } else {
        panic!("Blender exited with an error");
    }
}

fn gpu_count() {
    let count = match Nvml::init() {
        Ok(nvml) => nvml.device_count().unwrap_or(0),
        Err(_) => 0,
    };
    let data = json!({ "gpu_count": count });
    let _ = update(data);
}
