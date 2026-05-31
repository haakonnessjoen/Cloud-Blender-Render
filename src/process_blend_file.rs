use crate::db::{get_data, update};
use crate::frame_partition::{compute_partitions, Distribution, Partition};
use crate::web_push_notification;
use axum::{http::StatusCode, response::IntoResponse};
use lazy_static::lazy_static;
use nix::sys::signal::{kill, Signal::SIGTERM};
use nix::unistd::Pid;
use redis::{Client, JsonCommands};
use serde_json::{json, Value};
use socketioxide::extract::{Data, SocketRef};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

lazy_static! {
    // PIDs of all currently-running Blender child processes.
    static ref CHILD_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
}

fn redis_conn() -> redis::Connection {
    Client::open("redis://127.0.0.1:6379/")
        .unwrap()
        .get_connection()
        .unwrap()
}

/// Overwrite the whole parallel_status array (used at job start).
fn init_parallel_status(partitions: &[Partition]) {
    let arr: Vec<Value> = partitions
        .iter()
        .map(|p| {
            json!({
                "index": p.index,
                "gpu": p.index,
                "frames": p.frames_label,
                "state": "running",
                "render_stats": "",
                "retries": 0
            })
        })
        .collect();
    let mut con = redis_conn();
    let _: () = con
        .json_set("items", "$.parallel_status", &Value::Array(arr))
        .unwrap();
}

/// Set one field of one parallel_status entry.
fn set_status_field(index: usize, field: &str, value: Value) {
    let mut con = redis_conn();
    let path = format!("$.parallel_status[{}].{}", index, field);
    let _: () = con.json_set("items", &path, &value).unwrap();
}

/// Emit the current parallel_status array to the client.
fn emit_parallel_status(sock: &SocketRef) {
    let mut con = redis_conn();
    let raw: String = match con.json_get("items", "$.parallel_status") {
        Ok(r) => r,
        Err(_) => return,
    };
    // JSONPath get returns an array-wrapped result: [[...entries...]]
    if let Ok(Value::Array(outer)) = serde_json::from_str::<Value>(&raw) {
        if let Some(inner) = outer.into_iter().next() {
            let _ = sock.emit("parallel_status", &inner);
        }
    }
}

/// Read scene.frame_start / frame_end from the .blend via a headless probe.
fn extract_frame_range(blender_bin: &PathBuf, blend_path: &PathBuf) -> Option<(i64, i64)> {
    let output = std::process::Command::new(blender_bin)
        .arg("-b")
        .arg(blend_path)
        .arg("--python-expr")
        .arg("import bpy; s = bpy.context.scene; print('FRANGE', s.frame_start, s.frame_end)")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("FRANGE ") {
            let mut it = rest.split_whitespace();
            let s: i64 = it.next()?.parse().ok()?;
            let e: i64 = it.next()?.parse().ok()?;
            return Some((s, e));
        }
    }
    None
}

/// Resolve (start, end) for the current animation settings.
/// Returns None when the render is a single frame.
fn resolve_range(blender_bin: &PathBuf, blend_path: &PathBuf) -> Option<(i64, i64)> {
    let single = get_data("blender_settings.animation_sequence.single_frame.status") == "true";
    if single {
        return None;
    }
    let range = get_data("blender_settings.animation_sequence.range.status") == "true";
    if range {
        let s: i64 = get_data("blender_settings.animation_sequence.range.start_frame")
            .parse()
            .unwrap_or(1);
        let e: i64 = get_data("blender_settings.animation_sequence.range.end_frame")
            .parse()
            .unwrap_or(1);
        return Some((s, e));
    }
    // entire animation -> extract from the .blend
    extract_frame_range(blender_bin, blend_path)
}

pub fn start_render(socket: &SocketRef) {
    socket.on("blend_engine", {
        move |socket: SocketRef, Data::<Value>(data)| {
            let file_name = get_data("blend_file.file_name");
            let blend_path = PathBuf::from("blend-folder").join(&file_name);
            let blender_bin = PathBuf::from("blender/blender");

            let blend_process_status = get_data("render_status.is_rendering");
            let blend_file_exist = get_data("blend_file.is_present");

            // Clone the socket for the background thread
            let sock = socket.clone();

            if blend_process_status == "true" {
                if let Err(err) = sock.emit(
                    "blend-engine-error",
                    "Blender is already running. Cannot run duplicate task",
                ) {
                    eprintln!("Emit error: {:?}", err);
                    eprintln!("Blend_process_status - {}", err);
                };
            }

            if blend_file_exist == "false" {
                if let Err(err) = sock.emit(
                    "blend-engine-error",
                    "Blend file not exist. Please upload it first",
                ) {
                    eprintln!("Emit error: {:?}", err);
                    eprintln!("Blend_process_status - {}", err);
                };
            }

            if !data.is_object() || data.as_object().unwrap().is_empty() {
                if let Err(err) =
                    sock.emit("blend-engine-error", "Input data field cannot be empty")
                {
                    eprintln!("Emit error: {:?}", err);
                };
            }

            // Case 2: "blender-query" key must be present
            if !data.get("data_sync").is_some() {
                println!("key 'data_sync' is required");
                if let Err(err) = sock.emit(
                    "blend-engine-error",
                    "Non valid schema. Enter your blend command with a key `data_sync`",
                ) {
                    eprintln!("Emit error: {:?}", err);
                };
            }

            let data_sync = data.get("data_sync").unwrap();

            update(json!(data_sync)).unwrap();

            let data_sync_confirm = json!({
                "status" : true
            });
            sock.emit("data_sync_confirm", &data_sync_confirm).unwrap();

            let anime_query = get_data("anime_query");
            let engine_query = get_data("engine_query");

            if !(blend_file_exist == "true" && blend_process_status == "false") {
                return;
            }

            let parallel_enabled =
                get_data("blender_settings.parallel.enabled") == "true";
            let process_count: usize = get_data("blender_settings.parallel.process_count")
                .parse()
                .unwrap_or(1);
            let gpu_count: usize = get_data("gpu_count").parse().unwrap_or(1);
            let distribution =
                Distribution::from_str(&get_data("blender_settings.parallel.distribution"));

            let want_parallel =
                parallel_enabled && process_count > 1 && gpu_count > 1;

            let range = if want_parallel {
                resolve_range(&blender_bin, &blend_path)
            } else {
                None
            };

            match range {
                Some((s, e)) if want_parallel && (e - s + 1) > 1 => {
                    let count = process_count.min(gpu_count);
                    let partitions = compute_partitions(s, e, count, distribution);
                    render_parallel(&blender_bin, &blend_path, partitions, &engine_query, sock.clone());
                }
                _ => {
                    let blender_query = format!("{anime_query} {engine_query}");
                    render_task(&blender_bin, &blend_path, &blender_query, sock.clone());
                }
            }
        }
    });
}

pub fn render_task(
    blender_bin: &PathBuf,
    blend_path: &PathBuf,
    blender_query: &str,
    sock: SocketRef,
) {
    let blender_bin = blender_bin.clone();
    let blend_path = blend_path.clone();
    let blender_query = blender_query.to_string();

    let set_render_true = json!({
        "render_status" : {
        "is_rendering" : true
      }
    });

    update(set_render_true).unwrap();
    let rt_handle = tokio::runtime::Handle::current();

    thread::spawn(move || {
        let mut child = std::process::Command::new(&blender_bin)
            .arg("-b")
            .arg(&blend_path)
            .arg("-o")
            .arg("./output/")
            .arg("-P")
            .arg("cycles_optix_denoise_logic.py")
            .args(blender_query.split_whitespace())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to start Blender process");

        CHILD_PIDS.lock().unwrap().push(child.id());

        // Read Blender's stdout and emit each line
        if let Some(out) = child.stdout.take() {
            let reader = BufReader::new(out);
            for line in reader.lines().flatten() {
                // println!("{}", line);
                let payload = json!({ "render_stats": line });
                update(payload).unwrap();
                // if let Err(err) = sock.emit("blend_process", &payload) {
                //     eprintln!("Emit error: {:?}", err);
                // }
            }
        }

        // Optionally read stderr as well
        if let Some(err_out) = child.stderr.take() {
            let reader = BufReader::new(err_out);
            for line in reader.lines().flatten() {
                eprintln!("BLENDER ERR: {}", line);
                // let payload = json!({ "line": line });
                // let _ = sock.emit("blend_process", &payload);
                let payload = json!({ "render_stats": line });
                update(payload).unwrap();
            }
        }

        // Wait for Blender to finish
        let set_render_false = json!({
            "render_status" : {
                "is_rendering" : false
            }
        });

        update(set_render_false).unwrap();

        // Wait for Blender to finish
        let exit_status = child.wait().expect("Blender process failed");
        CHILD_PIDS.lock().unwrap().clear();
        let exit_message = json!({ "line": "Blender exited successfully", "finished" : true });

        update(exit_message.clone()).unwrap();

        if let Err(err) = sock.emit("blend_process", &exit_message) {
            eprintln!("Emit error: {:?}", err);
        }
        println!("Blender exited with status: {:?}", exit_status);

        

        rt_handle.spawn(async {
            web_push_notification::notify_render_complete().await;
        });
    });
}

pub fn render_parallel(
    blender_bin: &PathBuf,
    blend_path: &PathBuf,
    partitions: Vec<Partition>,
    engine_query: &str,
    sock: SocketRef,
) {
    let n = partitions.len();
    update(json!({ "render_status": { "is_rendering": true } })).unwrap();
    init_parallel_status(&partitions);
    CHILD_PIDS.lock().unwrap().clear();
    emit_parallel_status(&sock);

    let remaining = Arc::new(AtomicUsize::new(n));
    let rt_handle = tokio::runtime::Handle::current();

    for partition in partitions {
        spawn_process_worker(
            blender_bin.clone(),
            blend_path.clone(),
            partition,
            engine_query.to_string(),
            remaining.clone(),
            rt_handle.clone(),
            sock.clone(),
        );
    }
}

fn spawn_process_worker(
    blender_bin: PathBuf,
    blend_path: PathBuf,
    partition: Partition,
    engine_query: String,
    remaining: Arc<AtomicUsize>,
    rt_handle: tokio::runtime::Handle,
    sock: SocketRef,
) {
    thread::spawn(move || {
        let idx = partition.index;
        let mut attempt = 0;

        loop {
            let mut child = std::process::Command::new(&blender_bin)
                .env("CUDA_VISIBLE_DEVICES", idx.to_string())
                .arg("-b")
                .arg(&blend_path)
                .arg("-o")
                .arg("./output/")
                .arg("-P")
                .arg("cycles_optix_denoise_logic.py")
                .args(&partition.frame_args)
                .args(engine_query.split_whitespace())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("Failed to start Blender process");

            let pid = child.id();
            CHILD_PIDS.lock().unwrap().push(pid);
            set_status_field(idx, "state", json!("running"));
            emit_parallel_status(&sock);

            // Drain stderr on its own thread to avoid pipe-buffer deadlock.
            let stderr_handle = child.stderr.take().map(|err_out| {
                let idx = idx;
                thread::spawn(move || {
                    let reader = BufReader::new(err_out);
                    for line in reader.lines().flatten() {
                        eprintln!("BLENDER[{}] ERR: {}", idx, line);
                    }
                })
            });

            if let Some(out) = child.stdout.take() {
                let reader = BufReader::new(out);
                for line in reader.lines().flatten() {
                    set_status_field(idx, "render_stats", json!(line));
                    emit_parallel_status(&sock);
                }
            }
            if let Some(h) = stderr_handle {
                let _ = h.join();
            }

            let status = child.wait().expect("Blender process failed");

            // Remove this pid from the live list.
            {
                let mut pids = CHILD_PIDS.lock().unwrap();
                if let Some(pos) = pids.iter().position(|&p| p == pid) {
                    pids.remove(pos);
                }
            }

            if status.success() {
                set_status_field(idx, "state", json!("done"));
                emit_parallel_status(&sock);
                break;
            } else if attempt == 0 {
                attempt += 1;
                set_status_field(idx, "retries", json!(1));
                set_status_field(idx, "state", json!("retrying"));
                emit_parallel_status(&sock);
                continue;
            } else {
                set_status_field(idx, "state", json!("failed"));
                emit_parallel_status(&sock);
                break;
            }
        }

        // Last worker to finish flips the global flag and notifies.
        if remaining.fetch_sub(1, Ordering::SeqCst) == 1 {
            update(json!({ "render_status": { "is_rendering": false } })).unwrap();
            let exit_message = json!({ "line": "Blender exited successfully", "finished": true });
            update(exit_message.clone()).unwrap();
            let _ = sock.emit("blend_process", &exit_message);
            rt_handle.spawn(async {
                web_push_notification::notify_render_complete().await;
            });
        }
    });
}

pub async fn stop_render() -> impl IntoResponse {
    let pids: Vec<u32> = CHILD_PIDS.lock().unwrap().clone();

    if pids.is_empty() {
        return (StatusCode::BAD_REQUEST, "No render task are active".to_string());
    }

    let mut errors = Vec::new();
    for pid in &pids {
        if let Err(err) = kill(Pid::from_raw(*pid as i32), SIGTERM) {
            errors.push(format!("{}: {}", pid, err));
        }
    }
    CHILD_PIDS.lock().unwrap().clear();

    if errors.is_empty() {
        (StatusCode::OK, "Blender exited successfully".to_string())
    } else {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to cancel some renders: {}", errors.join(", ")),
        )
    }
}
