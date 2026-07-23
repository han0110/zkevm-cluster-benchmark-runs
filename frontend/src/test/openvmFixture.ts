/*
 * Builders for an openvm-shaped benchmark whose execution, segment, and recursion phases carry the
 * overlap flag, three consecutive overlap phases the zisk fixture cannot reach, for tests of the overlap
 * registry and the chain-band window math. Blocks are built per test because each case needs its own
 * precise phase windows.
 */

import type { Benchmark, Block, BlockNode, PhaseWindow } from '@/types/benchmark';
import { fixture } from '@/test/fixture';

// A phase window from start and duration in milliseconds.
export const win = (start_ms: number, dur_ms: number): PhaseWindow => ({ start_ms, dur_ms });

// A participating node holding the given per-phase windows.
export const openvmNode = (phases: (PhaseWindow | null)[]): BlockNode => ({
  phases,
  crashed_ms: null,
  crash_kind: null,
  participated: true,
});

// A successful block over the given nodes, proving for as long as its latest phase end.
export const openvmBlock = (name: string, nodes: BlockNode[]): Block => ({
  name,
  status: 'success',
  start_ms: 0,
  gas_used: 1_000_000,
  proving_ms: Math.max(0, ...nodes.flatMap(n => n.phases.flatMap(p => (p ? [p.start_ms + p.dur_ms] : [])))),
  proof_size: null,
  verification_time_ms: null,
  meta: {},
  nodes,
});

// The zisk fixture reshaped to the openvm preset, holding exactly the given blocks in one run.
export const openvmBenchmark = (blocks: Block[]): Benchmark => ({
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
    },
  },
  runs: [{ ...fixture.runs[0]!, blocks }],
});
