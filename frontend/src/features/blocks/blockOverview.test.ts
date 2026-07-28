import { describe, it, expect } from 'vitest';
import { blockOverviewFields } from '@/features/blocks/blockOverview';
import { fixture } from '@/test/fixture';
import { openvmBenchmark, openvmBlock, openvmNode, win } from '@/test/openvmFixture';
import { buildPhaseRegistry } from '@/utils/phases';
import type { Benchmark, PipelineStep } from '@/types/benchmark';

// The monolithic openvm kinds, enough to name which rows belong to the recursion phase.
const OPENVM_KINDS: PipelineStep[] = [
  { name: 'app_segment', label: 'Segment', phase: 'segment', paired: false },
  { name: 'leaf', label: 'Recursion', phase: 'recursion', paired: false },
  { name: 'internal', label: 'Internal Aggregation', phase: 'recursion', paired: false },
];

// An openvm document of one block, carrying the metered segment count the coordinator reports and the
// three recursion proofs its two nodes ran.
function openvmDocument(): Benchmark {
  const block = openvmBlock(
    'mainnet_25580000',
    [
      openvmNode([null, win(0, 500), win(100, 800), win(400, 900), null], [
        [1, 400, 200],
        [2, 700, 300],
      ]),
      openvmNode([null, win(0, 500), win(100, 800), null, null], [[1, 420, 180]]),
    ],
    { segments: 53 }
  );
  const base = openvmBenchmark([block]);
  return { ...base, software: { ...base.software, zkvm: { ...base.software.zkvm, pipeline: OPENVM_KINDS } } };
}

// The rendered value of one field, by key.
function rendered(bench: Benchmark, key: string): string {
  const registry = buildPhaseRegistry(bench);
  const block = bench.runs[0]!.blocks[0]!;
  return blockOverviewFields(bench).find(f => f.key === key)!.render(block, registry);
}

describe('blockOverviewFields', () => {
  it('reports the segment and recursion counts of an openvm block in full', () => {
    const bench = openvmDocument();
    expect(rendered(bench, 'segments')).toBe('53');
    // Two leaf proofs and one internal proof across the two nodes.
    expect(rendered(bench, 'recursions')).toBe('3');
  });

  it('sorts on the counts as numbers, so the table orders by trace size', () => {
    const bench = openvmDocument();
    const registry = buildPhaseRegistry(bench);
    const block = bench.runs[0]!.blocks[0]!;
    const fields = blockOverviewFields(bench);
    expect(fields.find(f => f.key === 'segments')!.sortValue(block, registry)).toBe(53);
    expect(fields.find(f => f.key === 'recursions')!.sortValue(block, registry)).toBe(3);
  });

  it('offers a zisk document its emulation figures and neither proof count', () => {
    const keys = blockOverviewFields(fixture).map(f => f.key);
    expect(keys).toEqual(['time', 'gas', 'proof_size', 'verify', 'input', 'steps', 'emulation']);
    expect(rendered(fixture, 'steps')).not.toBe('-');
  });

  it('offers an openvm document the proof counts and neither emulation figure', () => {
    const keys = blockOverviewFields(openvmDocument()).map(f => f.key);
    expect(keys).toEqual(['time', 'gas', 'proof_size', 'verify', 'input', 'segments', 'recursions']);
  });
});
