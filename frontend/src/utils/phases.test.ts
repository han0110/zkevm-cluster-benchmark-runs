import { describe, it, expect } from 'vitest';
import { fixture } from '@/test/fixture';
import { openvmBenchmark, openvmBlock, openvmNode, win } from '@/test/openvmFixture';
import { buildPhaseRegistry } from '@/utils/phases';
import type { Benchmark } from '@/types/benchmark';

describe('buildPhaseRegistry aggregator derivation', () => {
  it('derives the aggregator-only phase from the cluster shape', () => {
    const reg = buildPhaseRegistry(fixture);
    expect(reg.list.map(p => p.name)).toEqual(['input', 'emulation', 'commit', 'prove', 'aggregate']);
    // Only the aggregator node carries the final window, so 'aggregate' is present on some but not all
    // nodes, which is how it is identified.
    expect(reg.aggregatorPhase).toBe('aggregate');
  });

  it('searches every run for a clean block, not only the first run', () => {
    const crashed = fixture.runs[0]!.blocks.find(b => b.status === 'crashed')!;
    const clean = fixture.runs[0]!.blocks.find(b => b.name === '0001')!;
    // run0 holds only a crash here, so the aggregator can be read only from run1's clean block.
    const crossRun: Benchmark = {
      ...fixture,
      runs: [
        { ...fixture.runs[0]!, blocks: [crashed] },
        { ...fixture.runs[1]!, blocks: [clean] },
      ],
    };
    expect(buildPhaseRegistry(crossRun).aggregatorPhase).toBe('aggregate');
  });

  it('carries the overlap flags and derives the aggregator from the last subset phase', () => {
    const block = openvmBlock('b0', [
      openvmNode([win(0, 100), win(200, 200), win(400, 2000), win(2000, 1600), win(3600, 400)]),
      openvmNode([win(0, 200), win(200, 300), win(500, 2500), win(2500, 1300), null]),
      openvmNode([win(0, 150), win(200, 250), win(500, 2000), null, null]),
    ]);
    const reg = buildPhaseRegistry(openvmBenchmark([block]));
    expect(reg.list.map(p => p.overlap)).toEqual([false, true, true, true, false]);
    // Recursion also sits on a subset of the nodes here, so only the last subset phase is the
    // aggregator-only wrap.
    expect(reg.aggregatorPhase).toBe('wrap');
  });
});
