/*
 * Builders for an openvm-shaped benchmark whose execution, segment, and recursion phases carry the
 * overlap flag, three consecutive overlap phases the zisk fixture cannot reach, for tests of the overlap
 * registry, the chain-band window math, and the per-block figures read off the segment and recursion
 * proofs. Blocks are built per test because each case needs its own precise phase windows.
 */

import type { Benchmark, Block, BlockMeta, BlockNode, PipelineRowMeta, PhaseWindow, SubPhase } from '@/types/benchmark';
import { fixture } from '@/test/fixture';

// A phase window from start and duration in milliseconds.
export const win = (start_ms: number, dur_ms: number): PhaseWindow => ({ start_ms, dur_ms });

// The seven STARK sub-steps, in template order.
const SUB_STEP_NAMES = ['execute_preflight', 'trace_gen', 'main_trace_commit', 'logup_gkr', 'round0', 'mle_rounds', 'openings'];

// The sub-phase breakdown template, the seven sub-steps under the segment owner then under recursion,
// so slot i pairs with a block's sub-phase row index i.
export const openvmSubPhases: SubPhase[] = ['segment', 'recursion'].flatMap(phase =>
  SUB_STEP_NAMES.map(name => ({ name, label: name, phase }))
);

// A participating node holding the given per-phase windows, and the fine-pipeline rows a case that
// reads them needs.
export const openvmNode = (phases: (PhaseWindow | null)[], pipeline?: (number | PipelineRowMeta)[][]): BlockNode => ({
  phases,
  ...(pipeline ? { pipeline } : {}),
  crashed_ms: null,
  crash_kind: null,
  participated: true,
});

// A successful block over the given nodes, proving for as long as its latest phase end.
export const openvmBlock = (name: string, nodes: BlockNode[], meta: BlockMeta = {}): Block => ({
  name,
  status: 'success',
  start_ms: 0,
  gas_used: 1_000_000,
  proving_ms: Math.max(0, ...nodes.flatMap(n => n.phases.flatMap(p => (p ? [p.start_ms + p.dur_ms] : [])))),
  proof_size: null,
  verification_time_ms: null,
  meta,
  nodes,
});

// The zisk fixture reshaped to the openvm preset, holding exactly the given blocks in one run. The
// sub-phase template rides the document only when requested, mirroring a run whose logs carried per-item spans.
export const openvmBenchmark = (blocks: Block[], withSubPhases = false): Benchmark => ({
  ...fixture,
  software: {
    ...fixture.software,
    zkvm: {
      name: 'openvm',
      version: 'test',
      phases: [
        { name: 'input', label: 'Input Transfer' },
        { name: 'execution', label: 'Execution', overlap: true },
        { name: 'segment', label: 'Segment', overlap: true },
        { name: 'recursion', label: 'Recursion', overlap: true },
        { name: 'wrap', label: 'Wrap' },
      ],
      ...(withSubPhases ? { subphases: openvmSubPhases } : {}),
    },
  },
  runs: [{ ...fixture.runs[0]!, blocks }],
});
