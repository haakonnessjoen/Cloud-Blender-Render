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
