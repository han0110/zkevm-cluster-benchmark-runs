import { describe, it, expect } from 'vitest';
import { fixture } from '@/test/fixture';
import { buildPhaseRegistry } from '@/utils/phases';
import { decodePipeline, hasPipeline, rowPitch, rowWindow } from '@/utils/pipelineItems';
import type { Benchmark, Block, BlockNode, PipelineStep } from '@/types/benchmark';

const registry = buildPhaseRegistry(fixture);
const nodes = fixture.hardware.nodes;
const fixtureBlock = (name: string): Block => fixture.runs[0]!.blocks.find(b => b.name === name)!;

// A fixture variant carrying its own kind template, for shapes the committed fixture does not hold.
const withTemplate = (pipeline: PipelineStep[]): Benchmark => ({
  ...fixture,
  software: { ...fixture.software, zkvm: { ...fixture.software.zkvm, pipeline } },
});

// A synthetic block over the fixture's crashed block, its nodes swapped for the given ones.
const withNodes = (blockNodes: BlockNode[]): Block => ({ ...fixtureBlock('0001'), nodes: blockNodes });

const node = (pipeline: number[][], over: Partial<BlockNode> = {}): BlockNode => ({
  phases: [null, null, null, null, null],
  pipeline,
  crashed_ms: null,
  crash_kind: null,
  participated: true,
  ...over,
});

describe('decodePipeline', () => {
  it('decodes every wire arity into its segment windows', () => {
    const bench = withTemplate([
      { name: 'single', label: 'Single', phase: 'emulation', paired: false },
      { name: 'pair', label: 'Pair', phase: 'prove', paired: true },
    ]);
    const block = withNodes([
      node(
        [
          [1, 100, 200, 400, 500],
          [1, 1000, 300, 1500],
          [1, 2000, 250],
          [0, 3000, 40],
          [0, 4000],
        ],
        // A phase window carries the latest known end of the block, the dangling clamp, to 5000ms.
        { phases: [{ start_ms: 0, dur_ms: 5000 }, null, null, null, null] }
      ),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    expect(model.items.map(i => i.segments)).toEqual([
      // [k,s,d,s2,d2] is a full witness and compute pair.
      [
        { startSec: 0.1, endSec: 0.3 },
        { startSec: 0.4, endSec: 0.9 },
      ],
      // [k,s,d,s2] leaves the compute dangling, clamped to the latest known end.
      [
        { startSec: 1, endSec: 1.3 },
        { startSec: 1.5, endSec: 5, dangling: true },
      ],
      // [k,s,d] is one complete segment, on a paired kind as well as a singleton.
      [{ startSec: 2, endSec: 2.25 }],
      [{ startSec: 3, endSec: 3.04 }],
      // [k,s] is a dangling open, clamped likewise.
      [{ startSec: 4, endSec: 5, dangling: true }],
    ]);
    // The item envelope spans its first start to its last end.
    expect(model.items[0]).toMatchObject({ startSec: 0.1, endSec: 0.9, label: 'Pair' });
    expect(model.items[3]).toMatchObject({ startSec: 3, endSec: 3.04, label: 'Single' });
    // Paired flows from the template so the tooltip can name a lone completed segment's side.
    expect(model.items.map(i => i.paired)).toEqual([true, true, true, false, false]);
  });

  it('clamps a dangling segment to the crash moment on a crashed node', () => {
    const bench = withTemplate([
      { name: 'single', label: 'Single', phase: 'emulation', paired: false },
      { name: 'pair', label: 'Pair', phase: 'prove', paired: true },
    ]);
    const block = withNodes([
      // A sibling reaching further than the crash moment must not stretch the crashed node's clamp.
      node([[0, 0, 9000]]),
      node([[0, 1000], [1, 200, 300, 600]], { crashed_ms: 1500, crash_kind: 'crashed' }),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    const crashed = model.items.filter(i => i.nodeIndex === 1);
    expect(crashed.map(i => i.segments)).toEqual([
      [
        { startSec: 0.2, endSec: 0.5 },
        { startSec: 0.6, endSec: 1.5, dangling: true },
      ],
      [{ startSec: 1, endSec: 1.5, dangling: true }],
    ]);
  });

  it('clamps a dangling segment on a clean node to the latest known end of the block', () => {
    const bench = withTemplate([{ name: 'single', label: 'Single', phase: 'emulation', paired: false }]);
    const block = withNodes([
      // The envelope comes from a sibling's phase window ending at 2000ms.
      node([], { phases: [{ start_ms: 0, dur_ms: 2000 }, null, null, null, null] }),
      node([[0, 500]]),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    expect(model.items).toHaveLength(1);
    expect(model.items[0]!.segments).toEqual([{ startSec: 0.5, endSec: 2, dangling: true }]);
  });

  it('flattens nodes into one start-sorted list with node then kind tie-breaks', () => {
    const bench = withTemplate([
      { name: 'a', label: 'A', phase: 'emulation', paired: false },
      { name: 'b', label: 'B', phase: 'prove', paired: false },
    ]);
    const block = withNodes([
      node([[0, 100, 10], [1, 100, 10], [0, 900, 10]]),
      node([[1, 100, 10], [0, 500, 10]]),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    expect(model.items.map(i => [i.startSec, i.nodeIndex, i.kind])).toEqual([
      [0.1, 0, 0],
      [0.1, 0, 1],
      [0.1, 1, 1],
      [0.5, 1, 0],
      [0.9, 0, 0],
    ]);
  });

  it('attributes items to nodes by position', () => {
    const model = decodePipeline(fixture, fixtureBlock('0001'), nodes, registry);
    const byNode = new Map(model.items.map(i => [i.nodeIndex, i.nodeId]));
    expect(byNode.get(0)).toBe('node1');
    expect(byNode.get(2)).toBe('node3');
    // Only node3 recorded the aggregation row.
    expect(model.items.filter(i => i.kind === 2).map(i => i.nodeId)).toEqual(['node3']);
  });

  it('reads labels, phases, and colors from the template kind through the registry', () => {
    const model = decodePipeline(fixture, fixtureBlock('0001'), nodes, registry);
    const instance = model.items.find(i => i.kind === 1)!;
    expect(instance.label).toBe('Instance');
    expect(instance.phase).toBe('prove');
    expect(instance.color).toBe(registry.color('prove'));
  });

  it('lists the used phases once each in template order', () => {
    const model = decodePipeline(fixture, fixtureBlock('0001'), nodes, registry);
    expect(model.phasesUsed).toEqual(['emulation', 'prove', 'aggregate']);
    // A block whose rows never reach a kind drops its phase from the list.
    const bench = withTemplate([
      { name: 'a', label: 'A', phase: 'emulation', paired: false },
      { name: 'b', label: 'B', phase: 'prove', paired: false },
    ]);
    const partial = decodePipeline(bench, withNodes([node([[0, 100, 10]])]), nodes, registry);
    expect(partial.phasesUsed).toEqual(['emulation']);
  });

  it('skips malformed rows without dropping the valid ones', () => {
    const bench = withTemplate([{ name: 'single', label: 'Single', phase: 'emulation', paired: false }]);
    const block = withNodes([
      node([
        [0],
        [0, 1, 2, 3, 4, 5],
        [NaN, 100, 10],
        [0, Infinity, 10],
        [5, 100, 10],
        [-1, 100, 10],
        [0.5, 100, 10],
        [0, 100, 10],
      ]),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    expect(model.items).toHaveLength(1);
    expect(model.items[0]!.segments).toEqual([{ startSec: 0.1, endSec: 0.11 }]);
  });

  it('keeps the dangling clamp and endSec finite when a malformed row coexists', () => {
    const bench = withTemplate([{ name: 'single', label: 'Single', phase: 'emulation', paired: false }]);
    const block = withNodes([
      // The non-finite duration must not poison the clamp pre-scan, which reads the valid row's end.
      node([
        [0, 100, Infinity],
        [0, 500, 1500],
        [0, 1000],
      ]),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    expect(model.items.map(i => i.segments)).toEqual([
      [{ startSec: 0.5, endSec: 2 }],
      [{ startSec: 1, endSec: 2, dangling: true }],
    ]);
    expect(model.endSec).toBe(2);
  });

  it('reports the envelope end across all items', () => {
    const model = decodePipeline(fixture, fixtureBlock('0001'), nodes, registry);
    // The aggregation row on node3 ends last, at 4550 + 400 ms.
    expect(model.endSec).toBe(4.95);
    const empty = decodePipeline(fixture, fixtureBlock('0002'), nodes, registry);
    expect(empty.items).toEqual([]);
    expect(empty.endSec).toBe(0);
  });
});

describe('hasPipeline', () => {
  it('requires a non-empty template and rows on some node', () => {
    expect(hasPipeline(fixture, fixtureBlock('0001'))).toBe(true);
    // No node of this block carries rows.
    expect(hasPipeline(fixture, fixtureBlock('0002'))).toBe(false);
    // A template-less document never has a pipeline.
    const templateless: Benchmark = {
      ...fixture,
      software: { ...fixture.software, zkvm: { ...fixture.software.zkvm, pipeline: undefined } },
    };
    expect(hasPipeline(templateless, fixtureBlock('0001'))).toBe(false);
    // All-empty row arrays count as no rows.
    const emptied = withNodes([node([]), node([]), node([])]);
    expect(hasPipeline(fixture, emptied)).toBe(false);
  });
});

describe('rowPitch', () => {
  it('steps the pitch and bar down at the density thresholds', () => {
    expect(rowPitch(800)).toEqual({ pitch: 8, bar: 6 });
    expect(rowPitch(801)).toEqual({ pitch: 5, bar: 4 });
    expect(rowPitch(3000)).toEqual({ pitch: 5, bar: 4 });
    expect(rowPitch(3001)).toEqual({ pitch: 3, bar: 2 });
    expect(rowPitch(12000)).toEqual({ pitch: 3, bar: 2 });
    expect(rowPitch(12001)).toEqual({ pitch: 2, bar: 2 });
  });
});

describe('rowWindow', () => {
  it('covers the viewport plus overscan and clamps to the row range', () => {
    expect(rowWindow(0, 400, 8, 1000, 10)).toEqual({ start: 0, end: 60 });
    expect(rowWindow(800, 400, 8, 1000, 10)).toEqual({ start: 90, end: 160 });
    expect(rowWindow(7900, 400, 8, 1000, 10)).toEqual({ start: 977, end: 1000 });
    expect(rowWindow(0, 400, 8, 30, 10)).toEqual({ start: 0, end: 30 });
  });

  it('stays bounded by the viewport and overscan at any offset over 44000 rows', () => {
    const total = 44000;
    const { pitch } = rowPitch(total);
    const viewport = 900;
    const overscan = 150;
    const bound = Math.ceil(viewport / pitch) + 2 * overscan + 1;
    for (const scrollTop of [0, 1, 12345, 43210.5, total * pitch - viewport, total * pitch]) {
      const { start, end } = rowWindow(scrollTop, viewport, pitch, total, overscan);
      expect(end - start).toBeLessThanOrEqual(bound);
      expect(start).toBeGreaterThanOrEqual(0);
      expect(end).toBeLessThanOrEqual(total);
    }
  });
});
