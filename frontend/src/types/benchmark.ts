/* TypeScript shapes for the lean benchmark.json from the parse-benchmark tool. Cluster identity is held
 * once and runs accumulate, each with its own statistics, blocks, and telemetry. All time is integer ms
 * offset (block windows from the run epoch, phase windows from their block start). Telemetry is columnar
 * on an implicit one-second axis where tick i is i seconds from the run epoch. Phases are a per-zkVM
 * ordered preset, and counts and node ids are inferred from array lengths and positions. */

// Terminal proof outcome, a crash split from a timeout by crash reason.
export type BlockStatus = 'success' | 'crashed' | 'timeout';

// One entry of the run index built by enumerating the data directory.
export interface RunIndexEntry {
  id: string;
  url: string;
}

// Cluster hardware, assumed identical across nodes, node ids in order, counts inferred from lengths.
export interface Hardware {
  cpu_model: string | null;
  ram_gib: number | null;
  gpu_models: string[];
  nodes: string[];
}

// One proving-pipeline phase, `name` the stable key and `label` the display string, ordered by position.
export interface Phase {
  name: string;
  label: string;
  // An overlap phase runs concurrently with its neighbors. Omitted from the wire when false.
  overlap?: boolean;
}

// One fine-pipeline item kind, `phase` the owning phase-preset name that colors it. A paired kind
// carries a witness and a compute segment in one row, an unpaired kind a single segment.
export interface PipelineStep {
  name: string;
  label: string;
  phase: string;
  paired: boolean;
}

// The zkVM that produced the proofs, carrying its ordered phase preset.
export interface Zkvm {
  name: string;
  version: string;
  phases: Phase[];
  // Fine-pipeline kind template in wire order, indexed by BlockNode.pipeline rows. Absent on a
  // document generated before the pipeline was captured.
  pipeline?: PipelineStep[];
}

// The guest program that was proven.
export interface Guest {
  name: string;
  version: string;
}

export interface Software {
  zkvm: Zkvm;
  guest: Guest;
}

// One benchmark execution with its own statistics, blocks, and telemetry.
export interface Run {
  id: string;
  // First job start as unix epoch milliseconds.
  started_at: number;
  block_count: number;
  success_count: number;
  failure_count: number;
  statistics: Statistics;
  blocks: Block[];
  telemetry: Telemetry;
}

// Whole-run GPU rollup for one node, positioned to match Hardware.nodes, null where absent.
export interface NodeStats {
  max_temp: number | null;
  temp_throttle_seconds: number;
  mean_sm: number | null;
  mean_mem: number | null;
  peak_rxpci: number | null;
  peak_txpci: number | null;
}

export interface Statistics {
  mean_proving_ms: number | null;
  p50_proving_ms: number | null;
  p90_proving_ms: number | null;
  p95_proving_ms: number | null;
  p99_proving_ms: number | null;
  mean_gas_per_s: number | null;
  nodes: NodeStats[];
}

// A phase window relative to its block start, as offset and duration in milliseconds.
export interface PhaseWindow {
  start_ms: number;
  dur_ms: number;
}

// Optional per-item metadata carried as a pipeline row's trailing object. `id` numbers the item within
// its kind, and cpu_heavy and gpu_heavy name the segment index of each side of a paired item.
export interface PipelineRowMeta {
  id?: number;
  cpu_heavy?: number;
  gpu_heavy?: number;
}

// One node's contribution to a block, positioned to match Hardware.nodes. `phases[i]` aligns to
// Software.zkvm.phases[i] and the aggregate (last) window is non-null only on the aggregating node.
// `crashed_ms` is the block-start offset where this node was blamed for a crash, null otherwise.
export interface BlockNode {
  phases: (PhaseWindow | null)[];
  // Fine-pipeline items as variable-arity rows [kind, s, d, s2, d2] with trailing fields absent, times
  // in ms from the block start and `kind` indexing Software.zkvm.pipeline. Arity 3 is one complete
  // segment, arity 5 a witness and compute pair, and arities 2 and 4 leave the last segment dangling
  // where a crash cut it off. An item with metadata appends it after the numbers as one trailing
  // object. Rows sort by start then kind, and the field is absent when the node recorded none.
  pipeline?: (number | PipelineRowMeta)[][];
  crashed_ms: number | null;
  // How this node ended a crashed block, 'crashed' for the lost node and 'cancelled' for one stopped
  // after a sibling crashed. Null when unmarked.
  crash_kind: 'crashed' | 'cancelled' | null;
  // Whether this node took part. False marks a less-than-full-cluster proof, not comparable to a full run.
  participated: boolean;
}

// zkVM-specific per-block metadata, for zisk the input size, instance count, and step count.
export interface BlockMeta {
  input_size?: number;
  instances?: number;
  steps?: number;
}

// One cluster-log line within a block's proving window, role-tagged and at an offset from the block
// start, so the trace and the log console read on the same per-block time axis. Loaded on demand from
// the block's sidecar log file, not carried inline in benchmark.json.
export interface LogEntry {
  role: string;
  // Offset from the block start in microseconds, rendered to millisecond precision but sorted at full
  // precision so lines that share a millisecond keep their order.
  time: number;
  level: string;
  msg: string;
}

export interface Block {
  // The block identifier, the metric file name verbatim, unique within a run and possibly a long
  // fixture id.
  name: string;
  status: BlockStatus;
  // Block start offset from the run epoch in milliseconds.
  start_ms: number;
  gas_used: number | null;
  // Authoritative wall-clock proving time, null unless the proof succeeded.
  proving_ms: number | null;
  proof_size: number | null;
  verification_time_ms: number | null;
  meta: BlockMeta;
  nodes: BlockNode[];
}

// Display metadata for one telemetry metric.
export interface Metric {
  name: string;
  label: string;
  unit: string;
  max?: number;
}

// One node's telemetry as per-metric [gpu][tick] grids on the shared one-second axis (tick i is i
// seconds from the run epoch). A cell is null for an unread gpu or an unsampled second, and a metric
// entirely null on a node is omitted.
export interface NodeTelemetry {
  metrics: Record<string, (number | null)[][]>;
}

export interface Telemetry {
  metrics: Metric[];
  nodes: NodeTelemetry[];
}

export interface Benchmark {
  schema_version: number;
  hardware: Hardware;
  software: Software;
  // The benchmark identity shared by every run, unique among loaded documents.
  id: string;
  // Human-facing name and description read from the run directory's input benchmark.json.
  name: string;
  description: string;
  // One entry per execution. A patch appends a run, so the newest is not necessarily the last.
  runs: Run[];
}
