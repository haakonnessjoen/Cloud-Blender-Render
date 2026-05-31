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
    if (locked) return;
    const next = !parallel.enabled;
    set_parallel_enabled(next);
    // Schema seeds process_count at 1; enabling with 1 would silently fall
    // back to single-process on the backend, so bump to the minimum (2).
    if (next && parallel.process_count < 2) set_parallel_process_count(2);
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
