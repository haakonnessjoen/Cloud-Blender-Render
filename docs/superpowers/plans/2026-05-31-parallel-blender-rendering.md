# Parallel Blender Rendering Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the web UI launch N Blender processes (one pinned per GPU) that split a single animation's frames across GPUs, with selectable chunk/interleave distribution and per-process progress.

**Architecture:** Backend computes per-process frame arguments from a pure partition function, spawns N headless Blender processes each with `CUDA_VISIBLE_DEVICES=i`, tracks PIDs in a shared list, streams per-process status over a new `parallel_status` socket event, and auto-retries a failed process once. Frontend gains a parallel-config control and per-process status panels. Single-process behavior is preserved as the default fallback.

**Tech Stack:** Rust (axum, socketioxide, redis/DragonflyDB JSON, nvml-wrapper, nix, lazy_static), React + Zustand + immer, Blender 5.1.0 CLI.

---

## Design reference

Full design: `docs/superpowers/specs/2026-05-31-parallel-blender-rendering-design.md`.

## File Structure

**Backend**
- `src/frame_partition.rs` *(new)* — pure frame-splitting logic, unit-tested. One responsibility: turn `(start, end, count, distribution)` into per-process Blender frame args + labels.
- `src/db/app_state_schema.rs` *(modify)* — add `blender_settings.parallel`, `gpu_count`, `parallel_status`.
- `src/db/get_app_state.rs` *(modify)* — populate `gpu_count` via NVML on each `get_db`.
- `src/process_blend_file.rs` *(rewrite)* — single + parallel orchestration, PID list, retry, per-process status, multi-kill stop.
- `src/main.rs` *(modify)* — declare `mod frame_partition;`.

**Frontend**
- `frontend/src/component/Store.jsx` *(modify)* — `gpu_count`, `parallel_status`, parallel setters.
- `frontend/src/component/Parallelrender.jsx` *(new)* + `frontend/src/style/parallelrender.css` *(new)* — config UI.
- `frontend/src/component/Parallelstatus.jsx` *(new)* + `frontend/src/style/parallelstatus.css` *(new)* — per-process panels.
- `frontend/src/component/Controlpanel.jsx` *(modify)* — mount both, wire `parallel_status` socket listener.

## Conventions used by this plan

- Distribution string values are exactly `"chunks"` and `"interleave"` everywhere (state, backend, frontend values). UI labels them "Split chunks" / "Every Nth frame".
- Process index `i` equals the pinned GPU index (`CUDA_VISIBLE_DEVICES=i`).
- Build check backend: `cargo build` (run from repo root). There is no test runner besides `cargo test`.
- Build check frontend: `cd frontend && npm run build` and `npm run lint`.

---

### Task 1: Pure frame-partition module (TDD)

**Files:**
- Create: `src/frame_partition.rs`
- Modify: `src/main.rs` (add `mod frame_partition;` near the other `mod` lines at top)
- Test: inline `#[cfg(test)]` module in `src/frame_partition.rs`

- [ ] **Step 1: Write the failing tests**

Create `src/frame_partition.rs` with ONLY the test module and the public type/function signatures (no logic yet, `todo!()` body):

```rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Distribution {
    Chunks,
    Interleave,
}

impl Distribution {
    pub fn from_str(s: &str) -> Distribution {
        match s {
            "interleave" => Distribution::Interleave,
            _ => Distribution::Chunks,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Partition {
    pub index: usize,
    pub frame_args: Vec<String>,
    pub frames_label: String,
}

/// Split frames [start, end] across `count` processes.
/// Effective process count is clamped to the number of frames.
pub fn compute_partitions(
    start: i64,
    end: i64,
    count: usize,
    dist: Distribution,
) -> Vec<Partition> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(p: &Partition) -> Vec<&str> {
        p.frame_args.iter().map(|s| s.as_str()).collect()
    }

    #[test]
    fn chunks_even_split() {
        let parts = compute_partitions(1, 100, 4, Distribution::Chunks);
        assert_eq!(parts.len(), 4);
        assert_eq!(args(&parts[0]), ["-s", "1", "-e", "25", "-a"]);
        assert_eq!(args(&parts[1]), ["-s", "26", "-e", "50", "-a"]);
        assert_eq!(args(&parts[3]), ["-s", "76", "-e", "100", "-a"]);
        assert_eq!(parts[0].frames_label, "1-25");
    }

    #[test]
    fn chunks_uneven_split_front_loaded() {
        // 10 frames across 3 procs -> 4,3,3
        let parts = compute_partitions(1, 10, 3, Distribution::Chunks);
        assert_eq!(parts.len(), 3);
        assert_eq!(args(&parts[0]), ["-s", "1", "-e", "4", "-a"]);
        assert_eq!(args(&parts[1]), ["-s", "5", "-e", "7", "-a"]);
        assert_eq!(args(&parts[2]), ["-s", "8", "-e", "10", "-a"]);
    }

    #[test]
    fn count_clamped_to_frame_count() {
        // 2 frames, 8 procs requested -> only 2 partitions
        let parts = compute_partitions(5, 6, 8, Distribution::Chunks);
        assert_eq!(parts.len(), 2);
        assert_eq!(args(&parts[0]), ["-s", "5", "-e", "5", "-a"]);
        assert_eq!(parts[0].frames_label, "5");
        assert_eq!(args(&parts[1]), ["-s", "6", "-e", "6", "-a"]);
    }

    #[test]
    fn interleave_uses_frame_jump() {
        let parts = compute_partitions(1, 100, 4, Distribution::Interleave);
        assert_eq!(parts.len(), 4);
        assert_eq!(args(&parts[0]), ["-s", "1", "-e", "100", "-j", "4", "-a"]);
        assert_eq!(args(&parts[2]), ["-s", "3", "-e", "100", "-j", "4", "-a"]);
        assert_eq!(parts[2].frames_label, "3-100 step 4");
    }

    #[test]
    fn single_process_returns_one_partition() {
        let parts = compute_partitions(1, 50, 1, Distribution::Chunks);
        assert_eq!(parts.len(), 1);
        assert_eq!(args(&parts[0]), ["-s", "1", "-e", "50", "-a"]);
    }
}
```

Also add `mod frame_partition;` to `src/main.rs` alongside the existing `mod` declarations (top of file, e.g. after `mod delete_rendered_frames;`).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test frame_partition`
Expected: compiles, tests FAIL/panic at `todo!()` ("not yet implemented").

- [ ] **Step 3: Implement `compute_partitions`**

Replace the `todo!()` body with:

```rust
pub fn compute_partitions(
    start: i64,
    end: i64,
    count: usize,
    dist: Distribution,
) -> Vec<Partition> {
    let (lo, hi) = if start <= end { (start, end) } else { (end, start) };
    let frame_count = (hi - lo + 1).max(1) as usize;
    let n = count.clamp(1, frame_count);

    match dist {
        Distribution::Chunks => {
            let base = frame_count / n;
            let rem = frame_count % n;
            let mut parts = Vec::with_capacity(n);
            let mut cursor = lo;
            for i in 0..n {
                let len = base + if i < rem { 1 } else { 0 };
                let band_start = cursor;
                let band_end = cursor + len as i64 - 1;
                cursor = band_end + 1;
                let frames_label = if band_start == band_end {
                    band_start.to_string()
                } else {
                    format!("{}-{}", band_start, band_end)
                };
                parts.push(Partition {
                    index: i,
                    frame_args: vec![
                        "-s".into(),
                        band_start.to_string(),
                        "-e".into(),
                        band_end.to_string(),
                        "-a".into(),
                    ],
                    frames_label,
                });
            }
            parts
        }
        Distribution::Interleave => {
            let mut parts = Vec::with_capacity(n);
            for i in 0..n {
                let proc_start = lo + i as i64;
                parts.push(Partition {
                    index: i,
                    frame_args: vec![
                        "-s".into(),
                        proc_start.to_string(),
                        "-e".into(),
                        hi.to_string(),
                        "-j".into(),
                        n.to_string(),
                        "-a".into(),
                    ],
                    frames_label: format!("{}-{} step {}", proc_start, hi, n),
                });
            }
            parts
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test frame_partition`
Expected: all 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/frame_partition.rs src/main.rs
git commit -m "feat: pure frame-partition logic for parallel rendering"
```

---

### Task 2: State schema additions

**Files:**
- Modify: `src/db/app_state_schema.rs`

- [ ] **Step 1: Add parallel settings, gpu_count, parallel_status to the schema**

In `src/db/app_state_schema.rs`, inside the `json!({ ... })` literal:

Add `parallel` inside `blender_settings` (after the existing `cycle_device` field):

```rust
        "cycle_device" : "",
        "parallel" : {
          "enabled" : false,
          "process_count" : 1,
          "distribution" : "chunks"
        }
```

Add two top-level fields (next to `render_stats`):

```rust
      "render_stats" : "",
      "gpu_count" : 0,
      "parallel_status" : []
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build`
Expected: builds with no errors.

- [ ] **Step 3: Commit**

```bash
git add src/db/app_state_schema.rs
git commit -m "feat: add parallel settings and status to app state schema"
```

---

### Task 3: Populate gpu_count in get_db

**Files:**
- Modify: `src/db/get_app_state.rs`

- [ ] **Step 1: Add a gpu_count helper and call it**

At the top of `src/db/get_app_state.rs`, add the NVML import:

```rust
use nvml_wrapper::Nvml;
```

Add this function at the bottom of the file (mirrors `blender_version`):

```rust
fn gpu_count() {
    let count = match Nvml::init() {
        Ok(nvml) => nvml.device_count().unwrap_or(0),
        Err(_) => 0,
    };
    let data = json!({ "gpu_count": count });
    let _ = update(data);
}
```

In `get_db`, call it next to the existing setup calls:

```rust
pub async fn get_db() -> impl IntoResponse {
    check_blend_file();
    blender_version();
    gpu_count();
    // ... rest unchanged
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build`
Expected: builds with no errors.

- [ ] **Step 3: Manual verification (optional, needs running stack)**

If a stack with DragonflyDB is running locally, POST `/get_db` and confirm the response JSON contains `"gpu_count": <number>`. On a machine with no NVIDIA GPU this is `0`, which is correct.

- [ ] **Step 4: Commit**

```bash
git add src/db/get_app_state.rs
git commit -m "feat: expose detected gpu_count in app state"
```

---

### Task 4: Parallel orchestration in process_blend_file.rs

This is the largest task. It replaces the single static PID with a PID list, adds the parallel spawn path, per-process status updates, frame-range extraction, and retry-once. The existing single-process path is preserved as the fallback.

**Files:**
- Modify: `src/process_blend_file.rs`

- [ ] **Step 1: Replace imports and the PID global**

Replace the top of `src/process_blend_file.rs` (the `use` block and the `static CHILD_PID` line) with:

```rust
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
```

- [ ] **Step 2: Add redis status helpers**

Add these helper functions to the file (e.g. just below the `lazy_static!` block). They write into the `parallel_status` array element by index and re-emit the full array:

```rust
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
```

- [ ] **Step 3: Add the frame-range extractor**

Add this function to the file:

```rust
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
```

- [ ] **Step 4: Add range/frame-count resolution**

Add a helper that figures out the render's `(start, end)` from the current animation settings, returning `None` for single-frame (parallel not applicable):

```rust
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
```

- [ ] **Step 5: Rewrite the `start_render` handler to branch single vs parallel**

Replace the body of `start_render`'s closure from the line `let anime_query = get_data("anime_query");` through the end of the closure (the `if blend_file_exist == "true" && blend_process_status == "false" { render_task(...) }` block) with:

```rust
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
```

(Leave the earlier validation/`data_sync` parts of the closure unchanged. The variable `sock` is the cloned socket already present in the closure.)

- [ ] **Step 6: Update `render_task` (single process) to use the PID list**

In `render_task`, replace the single-PID store line:

```rust
        CHILD_PID.store(child.id(), Ordering::Relaxed);
```

with:

```rust
        CHILD_PIDS.lock().unwrap().push(child.id());
```

And at the very end of the spawned thread in `render_task` (after `child.wait()`), clear the list so a later stop doesn't target a dead PID. Add right after `let exit_status = child.wait().expect("Blender process failed");`:

```rust
        CHILD_PIDS.lock().unwrap().clear();
```

- [ ] **Step 7: Add `render_parallel` and the per-process worker**

Add these two functions to the file:

```rust
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
```

- [ ] **Step 8: Update `stop_render` to kill all PIDs**

Replace the whole `stop_render` function with:

```rust
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
```

- [ ] **Step 9: Verify it builds**

Run: `cargo build`
Expected: builds with no errors. Fix any unused-import warnings by removing imports the final code does not use (the `Data`/`Value`/`AtomicUsize` etc. listed in Step 1 are all used).

- [ ] **Step 10: Commit**

```bash
git add src/process_blend_file.rs
git commit -m "feat: parallel multi-GPU render orchestration with retry and multi-kill stop"
```

---

### Task 5: Frontend store — gpu_count, parallel_status, setters

**Files:**
- Modify: `frontend/src/component/Store.jsx`

- [ ] **Step 1: Add initial state fields**

In `Store.jsx`, add to the initial object (near `settings_box: false,`):

```jsx
  gpu_count: 0,
  parallel_status: [],
```

- [ ] **Step 2: Add parallel setters and a parallel_status setter**

Add these setters inside the store (next to the other `set_*` functions):

```jsx
  set_parallel_enabled: (value) => {
    set(
      produce((state) => {
        state.blender_settings.parallel.enabled = value;
      })
    );
  },
  set_parallel_process_count: (value) => {
    set(
      produce((state) => {
        state.blender_settings.parallel.process_count = Math.max(1, Number(value) || 1);
      })
    );
  },
  set_parallel_distribution: (value) => {
    set(
      produce((state) => {
        state.blender_settings.parallel.distribution = value;
      })
    );
  },
  set_parallel_status: (value) => {
    set(
      produce((state) => {
        state.parallel_status = value;
      })
    );
  },
```

- [ ] **Step 3: Verify it builds and lints**

Run: `cd frontend && npm run build && npm run lint`
Expected: build succeeds, lint clean.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/component/Store.jsx
git commit -m "feat: store state and setters for parallel rendering"
```

---

### Task 6: Parallel config UI component

**Files:**
- Create: `frontend/src/component/Parallelrender.jsx`
- Create: `frontend/src/style/parallelrender.css`

- [ ] **Step 1: Create the component**

Create `frontend/src/component/Parallelrender.jsx`:

```jsx
import "../style/parallelrender.css";
import central_store from "./Store";

export default function Parallelrender() {
  const {
    set_parallel_enabled,
    set_parallel_process_count,
    set_parallel_distribution,
  } = central_store();

  const gpu_count = central_store((state) => state.gpu_count);
  const blend_file_present = central_store((state) => state.blend_file.is_present);
  const render_status = central_store((state) => state.render_status.is_rendering);
  const single_frame = central_store(
    (state) => state.blender_settings.animation_sequence.single_frame.status
  );
  const parallel = central_store((state) => state.blender_settings.parallel);

  // Parallel only makes sense with >1 GPU and a multi-frame render.
  const available = gpu_count > 1 && !single_frame;
  const locked = !blend_file_present || render_status || !available;

  const toggle = () => {
    if (!locked) set_parallel_enabled(!parallel.enabled);
  };

  const note = () => {
    if (gpu_count <= 1) return "1 GPU detected — parallel rendering needs 2+ GPUs";
    if (single_frame) return "Parallel rendering applies to multi-frame renders only";
    return `${gpu_count} GPUs available`;
  };

  // Process count options: 2..gpu_count
  const options = [];
  for (let i = 2; i <= gpu_count; i++) options.push(i);

  return (
    <div className={`parallel-container ${locked ? "dim-opacity" : ""}`}>
      <div className="parallel-top">
        <p>Parallel rendering</p>
        <div
          className={`parallel-toggle ${parallel.enabled ? "parallel-toggle-on" : ""}`}
          onClick={toggle}
        >
          {parallel.enabled ? "On" : "Off"}
        </div>
      </div>
      <div className="parallel-note">{note()}</div>

      {parallel.enabled && available && (
        <div className="parallel-body">
          <div className="parallel-row">
            <label>GPUs / processes</label>
            <select
              value={parallel.process_count}
              disabled={locked}
              onChange={(e) => set_parallel_process_count(e.target.value)}
            >
              {options.map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </div>
          <div className="parallel-row">
            <label>Distribution</label>
            <div className="parallel-dist">
              <span
                className={parallel.distribution === "chunks" ? "active" : ""}
                onClick={() => !locked && set_parallel_distribution("chunks")}
              >
                Split chunks
              </span>
              <span
                className={parallel.distribution === "interleave" ? "active" : ""}
                onClick={() => !locked && set_parallel_distribution("interleave")}
              >
                Every Nth frame
              </span>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
```

- [ ] **Step 2: Create the stylesheet**

Create `frontend/src/style/parallelrender.css`:

```css
.parallel-container {
  margin-top: 16px;
  padding: 12px;
  border-radius: 8px;
  background: #1c1c1c;
  color: #ddd;
  font-size: 14px;
}
.parallel-top {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.parallel-toggle {
  padding: 4px 12px;
  border-radius: 6px;
  background: #333;
  cursor: pointer;
  user-select: none;
}
.parallel-toggle-on {
  background: #2e7d32;
  color: #fff;
}
.parallel-note {
  margin-top: 6px;
  font-size: 12px;
  opacity: 0.7;
}
.parallel-body {
  margin-top: 10px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.parallel-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
}
.parallel-row select {
  background: #262626;
  color: #ddd;
  border: 1px solid #444;
  border-radius: 6px;
  padding: 4px 8px;
}
.parallel-dist {
  display: flex;
  gap: 8px;
}
.parallel-dist span {
  padding: 4px 10px;
  border-radius: 6px;
  background: #262626;
  cursor: pointer;
  user-select: none;
}
.parallel-dist span.active {
  background: #1565c0;
  color: #fff;
}
.dim-opacity {
  opacity: 0.4;
  pointer-events: none;
}
```

- [ ] **Step 3: Verify it builds and lints**

Run: `cd frontend && npm run build && npm run lint`
Expected: build succeeds, lint clean. (Component is not yet mounted; that happens in Task 8.)

- [ ] **Step 4: Commit**

```bash
git add frontend/src/component/Parallelrender.jsx frontend/src/style/parallelrender.css
git commit -m "feat: parallel rendering config UI component"
```

---

### Task 7: Per-process status panels component

**Files:**
- Create: `frontend/src/component/Parallelstatus.jsx`
- Create: `frontend/src/style/parallelstatus.css`

- [ ] **Step 1: Create the component**

Create `frontend/src/component/Parallelstatus.jsx`:

```jsx
import "../style/parallelstatus.css";
import central_store from "./Store";

const STATE_LABEL = {
  running: "Rendering",
  done: "Done",
  failed: "Failed",
  retrying: "Retrying",
};

export default function Parallelstatus() {
  const parallel_status = central_store((state) => state.parallel_status);

  if (!parallel_status || parallel_status.length === 0) return null;

  return (
    <div className="pstatus-container">
      {parallel_status.map((p) => (
        <div key={p.index} className={`pstatus-card pstatus-${p.state}`}>
          <div className="pstatus-head">
            <span className="pstatus-gpu">GPU {p.gpu}</span>
            <span className="pstatus-badge">{STATE_LABEL[p.state] || p.state}</span>
          </div>
          <div className="pstatus-frames">Frames: {p.frames}</div>
          <div className="pstatus-log">{p.render_stats}</div>
        </div>
      ))}
    </div>
  );
}
```

- [ ] **Step 2: Create the stylesheet**

Create `frontend/src/style/parallelstatus.css`:

```css
.pstatus-container {
  display: flex;
  flex-direction: column;
  gap: 8px;
  margin-top: 12px;
}
.pstatus-card {
  padding: 10px;
  border-radius: 8px;
  background: #1c1c1c;
  border-left: 4px solid #555;
  color: #ddd;
  font-size: 13px;
}
.pstatus-running { border-left-color: #1565c0; }
.pstatus-done { border-left-color: #2e7d32; }
.pstatus-failed { border-left-color: #c62828; }
.pstatus-retrying { border-left-color: #f9a825; }
.pstatus-head {
  display: flex;
  justify-content: space-between;
  font-weight: 600;
}
.pstatus-frames {
  margin-top: 4px;
  opacity: 0.8;
}
.pstatus-log {
  margin-top: 4px;
  font-family: monospace;
  font-size: 11px;
  opacity: 0.7;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
```

- [ ] **Step 3: Verify it builds and lints**

Run: `cd frontend && npm run build && npm run lint`
Expected: build succeeds, lint clean.

- [ ] **Step 4: Commit**

```bash
git add frontend/src/component/Parallelstatus.jsx frontend/src/style/parallelstatus.css
git commit -m "feat: per-process parallel render status panels"
```

---

### Task 8: Mount components and wire the socket listener

**Files:**
- Modify: `frontend/src/component/Controlpanel.jsx`

- [ ] **Step 1: Import the new components and the store setter**

At the top of `Controlpanel.jsx`, add imports:

```jsx
import Parallelrender from "./Parallelrender";
import Parallelstatus from "./Parallelstatus";
```

- [ ] **Step 2: Subscribe to the parallel_status socket event**

In `Controlpanel`, extend the existing `useEffect` that registers socket handlers. Replace:

```jsx
  useEffect(() => {
    socket.on("data_sync_confirm", (res) => {
      if (res.status === true) {
        fetch_data();
      }
    });
  }, []);
```

with:

```jsx
  const set_parallel_status = central_store((state) => state.set_parallel_status);

  useEffect(() => {
    socket.on("data_sync_confirm", (res) => {
      if (res.status === true) {
        fetch_data();
      }
    });
    socket.on("parallel_status", (arr) => {
      if (Array.isArray(arr)) set_parallel_status(arr);
    });
    return () => {
      socket.off("parallel_status");
    };
  }, []);
```

- [ ] **Step 3: Mount the config component in the control input**

In the `Control_input` component at the bottom of the file, add `Parallelrender` after `Animationtype`:

```jsx
const Control_input = () => {
  return (
    <>
      <Fileinput />
      <Animationtype />
      <Enginetype />
      <Parallelrender />
    </>
  );
};
```

- [ ] **Step 4: Mount the status panels**

Inside the returned JSX of `Controlpanel`, render `Parallelstatus` below the render start/stop control. Add it right after the closing `</div>` of `render-start-stop` (before `toggle-panel-box`):

```jsx
          <Parallelstatus />
```

- [ ] **Step 5: Verify it builds and lints**

Run: `cd frontend && npm run build && npm run lint`
Expected: build succeeds, lint clean.

- [ ] **Step 6: Commit**

```bash
git add frontend/src/component/Controlpanel.jsx
git commit -m "feat: mount parallel render UI and wire status socket"
```

---

### Task 9: End-to-end manual verification

No automated test can cover GPU pinning + Blender execution. Verify manually on a multi-GPU RunPod instance (or document that it was deferred to deploy).

**Files:** none (verification only)

- [ ] **Step 1: Backend build is clean**

Run: `cargo build`
Expected: no errors.

- [ ] **Step 2: Frontend build is clean**

Run: `cd frontend && npm run build`
Expected: dist produced, no errors. (The Rust binary embeds `frontend/dist` via `include_dir!`, so the frontend must be built before the backend is rebuilt for release.)

- [ ] **Step 3: Manual smoke test (on multi-GPU host)**

1. Upload a multi-frame `.blend`, select Cycles + an animation mode (Entire or Range).
2. Confirm the "Parallel rendering" panel shows the GPU count and is enabled.
3. Enable it, pick process count = GPU count, choose "Split chunks".
4. Start render. Confirm:
   - `nvidia-smi` shows one Blender process pinned per GPU (distinct PIDs, each on its own GPU).
   - Per-process panels appear, one per GPU, each showing its frame band and advancing log lines.
   - Output frames for the full range land in `./output/` with no gaps and no duplicates.
5. Repeat with "Every Nth frame" and confirm frames are interleaved across GPUs but the full set still completes.
6. Kill one Blender process mid-render (`kill <pid>`); confirm its panel shows "Retrying" then resumes, while others keep going.
7. Press Stop; confirm all Blender processes terminate.

- [ ] **Step 4: Final commit (if any doc updates)**

```bash
git add -A
git commit -m "docs: parallel rendering verification notes" || true
```

---

## Self-review notes

- **Spec coverage:** pinned mode (Task 4 env pinning), distribution chunks+interleave (Task 1 + UI Task 6), auto-extract range (Task 4 Step 3), per-process panels (Task 7), retry-once + survivors continue (Task 4 Step 7), gpu_count gate (Task 3 + Task 6), multi-kill stop (Task 4 Step 8), schema (Task 2) — all mapped.
- **Single-process fallback** preserved in Task 4 Step 5 (the `_ =>` arm calls existing `render_task`).
- **Type consistency:** `Distribution`/`Partition`/`compute_partitions` defined in Task 1 and used unchanged in Task 4; `parallel_status` entry shape defined in Task 4 Step 2 matches the fields read in Task 7.
- **Known limitation:** stdout/stderr drain uses a dedicated stderr thread to avoid pipe deadlock (Task 4 Step 7). Live preview stays global per the spec.
