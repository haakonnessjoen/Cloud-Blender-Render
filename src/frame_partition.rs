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
