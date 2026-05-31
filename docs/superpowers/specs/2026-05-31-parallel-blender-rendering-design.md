# Parallel Blender Rendering — Design

Date: 2026-05-31
Status: Approved (pending implementation plan)

## Goal

Let the web UI launch more than one Blender process to share the work of rendering a
single `.blend` animation across multiple GPUs. Each Blender process is pinned to one
GPU. The number of parallel processes is capped at the number of GPUs the container can
see. Parallel mode is only meaningful for multi-frame renders (animations), so the UI
only offers it when more than one frame will be rendered.

## Background / corrections to the original idea

- **Blender "Network Render" no longer exists.** The addon was removed in Blender 2.80
  (2019). This project ships Blender **5.1.0** (see `Dockerfile` — note the binary is
  5.1.0, not 5.0.1 as a commit message states). There is no built-in network/distributed
  render in modern Blender. We therefore parallelize by **frame splitting**: spawn N
  headless Blender processes, each rendering a disjoint subset of frames, each pinned to
  one GPU, all writing to the same `./output/` folder. Output filenames already include
  the frame number, so disjoint frame sets never collide.
- **Multi-process only helps animations.** A single frame cannot be split across separate
  processes in the modern Blender CLI; multi-GPU on one frame is handled *inside* one
  Cycles process, not across processes. So parallel multi-process mode is enabled only
  when the render covers more than one frame.
- **GPU sharing (oversubscription) is out of scope.** Two Blender processes on one GPU
  rarely render faster (one Cycles process already saturates GPU compute) and doubles VRAM
  use (OOM risk). We build **pinned mode only**: one process per GPU.

## Decisions

| Topic | Decision |
| --- | --- |
| GPU mode | Pinned only. One Blender per GPU via `CUDA_VISIBLE_DEVICES=i`. |
| Max processes | `min(process_count, gpu_count, frame_count)`. |
| Distribution | User-selectable: `chunks` (contiguous bands) or `interleave` (every Nth frame). |
| Entire-animation range | Auto-extracted from the `.blend` before spawning. |
| Progress UI | Per-process panels (one per GPU). |
| Failure handling | Survivors keep rendering; a failed chunk auto-retries once, then is flagged failed. |

## Core mechanism: frame splitting

Let the render cover frames `[s, e]` and let `N = min(process_count, gpu_count, frame_count)`.

Each process `i` (0-indexed) is spawned with environment `CUDA_VISIBLE_DEVICES=i` so that
Blender sees exactly one real GPU (index `i`). The denoise Python script
(`cycles_optix_denoise_logic.py`) needs **no change**: both OptiX and CUDA honor
`CUDA_VISIBLE_DEVICES`, so its "enable all OptiX/CUDA devices" loop enables only the single
visible device.

Per-process frame arguments:

- **chunks**: split `[s, e]` into N contiguous bands; process `i` gets
  `-s <band_start> -e <band_end> -a`.
- **interleave**: process `i` gets `-s <s+i> -e <e> -j <N> -a` (`-j` = frame jump / step).
  Guard: only spawn process `i` if `s + i <= e`.

Full command per process (mirrors the current single-process invocation):

```
CUDA_VISIBLE_DEVICES=i blender -b <blend_path> -o ./output/ \
  -P cycles_optix_denoise_logic.py <frame_args> <engine_query>
```

### Auto-extracting the frame range

The UI's three animation modes map to ranges:

- **Single frame** → 1 frame → parallel disabled (never reaches the parallel path).
- **Range** → `s`/`e` come directly from the user's range inputs.
- **Entire animation** (`-a` alone) → range lives inside the `.blend`. Before spawning,
  run one quick headless probe:

  ```
  blender -b <blend_path> --python-expr \
    "import bpy; s=bpy.context.scene; print('FRANGE', s.frame_start, s.frame_end)"
  ```

  Parse `FRANGE <start> <end>` from stdout. Cost ~1–2s, run **once** (not per process).
  If the extracted range is a single frame, `N` clamps to 1 and the job runs as a normal
  single process.

## Backend design

### State schema (`src/db/app_state_schema.rs`)

Add to `blender_settings`:

```jsonc
"parallel": {
  "enabled": false,
  "process_count": 1,
  "distribution": "chunks"   // "chunks" | "interleave"
}
```

Add at top level:

```jsonc
"gpu_count": 0,
"parallel_status": []        // runtime, per-process; see below
```

Each `parallel_status` entry:

```jsonc
{
  "index": 0,                // process index
  "gpu": 0,                  // pinned GPU index (== index)
  "frames": "1-25",          // human-readable assigned set ("1-25" or "1,4,7,…")
  "state": "running",        // "running" | "done" | "failed" | "retrying"
  "render_stats": "",        // last log line from this process
  "retries": 0
}
```

### GPU count (`src/db/get_app_state.rs`)

In `get_db`, populate `gpu_count` via NVML `device_count()` (NVML already a dependency,
used in `machine_lookup.rs`), the same way `blender_version()` is written today. No new
route needed — the frontend reads `gpu_count` from the fetched app state. If NVML init
fails, write `0`.

### Render orchestration (`src/process_blend_file.rs` — major rewrite)

On the `blend_engine` socket event:

1. Keep all existing validation (file present, not already rendering, schema checks).
2. Read `blender_settings.parallel`.
3. **Fallback to single process** (current behavior, unchanged) when any of:
   `parallel.enabled == false`, `process_count <= 1`, or computed `frame_count <= 1`.
4. **Parallel path**:
   1. Determine `[s, e]`: from range inputs, or auto-extract for entire-animation.
   2. `N = min(process_count, gpu_count, frame_count)`.
   3. Compute per-process frame args (chunks or interleave) and the human-readable
      `frames` label.
   4. Initialize `parallel_status` array (N entries, `state: "running"`).
   5. Spawn N processes, each with `CUDA_VISIBLE_DEVICES=i` and its frame args. Store
      each PID in a shared **PID list** (replaces the single `CHILD_PID: AtomicU32`; use a
      `Mutex<Vec<u32>>` or equivalent).
   6. Per process, a reader thread consumes stdout/stderr, updates
      `parallel_status[i].render_stats` with the latest line, and the backend emits the
      `parallel_status` socket event (full array) on update.
   7. **Retry-once**: when a process exits non-zero, if `retries == 0`, set
      `state: "retrying"`, increment `retries`, respawn with the same args/GPU, set back to
      `running`. On a second failure, set `state: "failed"` (its frames left unrendered).
   8. When **all** processes reach a terminal state (`done` or `failed`), set
      `render_status.is_rendering = false` and fire `web_push_notification::notify_render_complete()`.

### Stop (`stop_render`)

SIGTERM **every** PID in the list (today it kills one). Clear the list afterward.

### Stats plumbing

- Parallel mode emits the new `parallel_status` socket event (full array) on each update.
- Single-process mode keeps the existing `render_stats` redis-pubsub → socket path
  unchanged for backward compatibility.
- **Live preview** (`live_image_preview.rs`) is unchanged and stays **global**: the
  `./output` watcher streams whichever frame was written last, regardless of which GPU
  produced it. Per-panel previews are out of scope.

## Frontend design

| File | Change |
| --- | --- |
| `Store.jsx` | Add `gpu_count` (default 0), `parallel_status` (default `[]`), and setters for `parallel.enabled`, `parallel.process_count`, `parallel.distribution`. |
| `Parallelrender.jsx` *(new)* | Enable toggle, process-count selector (`1..gpu_count`), distribution radio (`chunks` / `every Nth`). |
| `Parallelstatus.jsx` *(new)* | Renders N panels from `parallel_status`: GPU #, assigned frames, state badge (running/done/failed/retrying), last log line. |
| `Controlpanel.jsx` | Mount `Parallelrender` in `Control_input`; render `Parallelstatus` while a parallel job runs. |
| Socket wiring | Listen for the `parallel_status` event and write it into the store. |
| CSS | New stylesheets for the two new components, matching existing component style. |

### UI enable rule

The parallel section is interactive only when **`gpu_count > 1`** AND the current
animation mode is **not single-frame**. When `gpu_count <= 1`, the section is disabled and
shows "1 GPU detected — parallel rendering needs 2+ GPUs". Frame count for the gate:

- single frame → 1 → disabled
- range → `end - start + 1`
- entire animation → treated as multi-frame (enabled); if it turns out to be 1 frame, the
  backend self-corrects by clamping `N` to 1.

`parallel` settings travel inside the existing `blender_settings` payload already sent in
`data_sync` on the `blend_engine` emit — no new request shape.

## Files touched (summary)

**Backend**
- `src/db/app_state_schema.rs` — schema additions
- `src/db/get_app_state.rs` — populate `gpu_count`
- `src/process_blend_file.rs` — parallel orchestration, PID list, retry, multi-kill stop, per-process stats

**Frontend**
- `frontend/src/component/Store.jsx`
- `frontend/src/component/Parallelrender.jsx` *(new)* + CSS
- `frontend/src/component/Parallelstatus.jsx` *(new)* + CSS
- `frontend/src/component/Controlpanel.jsx`
- Socket listener wiring (`Socket.jsx` or component-level)

## Risks / notes

- `CUDA_VISIBLE_DEVICES` pins both CUDA and OptiX; denoise script unchanged.
- Range auto-extract adds ~1–2s startup, run once.
- Interleave guard: only spawn process `i` when `s + i <= e`.
- Single global `is_rendering` flag flips false only after all processes are terminal.
- No output-file collisions: frame sets are disjoint and filenames carry frame numbers.
