import { describe, it, expect } from 'vitest';
import { fixture } from '@/test/fixture';
import { openvmBenchmark, openvmSubPhases } from '@/test/openvmFixture';
import { buildPhaseRegistry, subPhaseKey } from '@/utils/phases';
import { decodePipeline, hasPipeline, packRows, rowPitch, rowWindow } from '@/utils/pipelineItems';
import type { Benchmark, Block, BlockNode, PipelineRowMeta, PipelineStep } from '@/types/benchmark';

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

const node = (pipeline: (number | PipelineRowMeta)[][], over: Partial<BlockNode> = {}): BlockNode => ({
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
  });

  it('decodes a trailing metadata object into the id and heavy side markers', () => {
    const bench = withTemplate([
      { name: 'single', label: 'Single', phase: 'emulation', paired: false },
      { name: 'pair', label: 'Pair', phase: 'prove', paired: true },
    ]);
    const block = withNodes([
      node([
        [0, 100, 200, { id: 12 }],
        [1, 400, 100, 600, 200, { id: 3, cpu_heavy: 0, gpu_heavy: 1 }],
        [0, 900, 50],
        [0, 1200, 40, { id: 'x' as unknown as number }],
      ]),
    ]);
    const model = decodePipeline(bench, block, nodes, registry);
    // The metadata object does not count toward the row arity, a metadata-free row leaves every
    // field undefined, and a non-numeric field is dropped rather than surfaced.
    expect(model.items.map(i => [i.id, i.cpuHeavy, i.gpuHeavy])).toEqual([
      [12, undefined, undefined],
      [3, 0, 1],
      [undefined, undefined, undefined],
      [undefined, undefined, undefined],
    ]);
    expect(model.items[1]!.segments).toEqual([
      { startSec: 0.4, endSec: 0.5 },
      { startSec: 0.6, endSec: 0.8 },
    ]);
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
    // A template-less document never has a pipeline, whether the template is absent or empty.
    const templateless: Benchmark = {
      ...fixture,
      software: { ...fixture.software, zkvm: { ...fixture.software.zkvm, pipeline: undefined } },
    };
    expect(hasPipeline(templateless, fixtureBlock('0001'))).toBe(false);
    expect(hasPipeline(withTemplate([]), fixtureBlock('0001'))).toBe(false);
    // All-empty row arrays count as no rows.
    const emptied = withNodes([node([]), node([]), node([])]);
    expect(hasPipeline(fixture, emptied)).toBe(false);
  });
});

describe('packRows', () => {
  // An openvm-shaped template carrying the execution, fast-forward, and segment kinds the packer folds.
  const openvm = withTemplate([
    { name: 'execution', label: 'Metered Execution', phase: 'execution', paired: false },
    { name: 'fastfwd', label: 'Fast Forward', phase: 'execution', paired: false },
    { name: 'app_segment', label: 'Segment', phase: 'segment', paired: false },
    { name: 'leaf', label: 'Recursion', phase: 'recursion', paired: false },
  ]);
  const pack = (block: Block) => packRows(decodePipeline(openvm, block, nodes, registry).items);

  it('folds execution and fast-forward onto the app segment of the same id on its node', () => {
    const rows = pack(
      withNodes([
        node([
          // The execution, fast-forward, and segment items share the id 0, the segment index, and
          // fold onto one row in start order.
          [0, 100, 50, { id: 0 }],
          [1, 300, 20, { id: 0 }],
          [2, 320, 200, { id: 0 }],
          // A later segment carries a different id, so it keeps its own row.
          [2, 700, 100, { id: 16 }],
        ]),
      ])
    );
    expect(rows.map(r => r.items.map(i => [i.name, i.id]))).toEqual([
      [
        ['execution', 0],
        ['fastfwd', 0],
        ['app_segment', 0],
      ],
      [['app_segment', 16]],
    ]);
    // The merged row takes the execution start, not the segment start it leads into.
    expect(rows[0]!.startSec).toBe(0.1);
    expect(rows[1]!.startSec).toBe(0.7);
  });

  it('leads with the fast-forward when a segment has no execution item', () => {
    // A segment whose executor send the lossy capture dropped has no execution item, so its row
    // leads with the fast-forward.
    const rows = pack(withNodes([node([[1, 300, 20, { id: 0 }], [2, 320, 200, { id: 0 }]])]));
    expect(rows.map(r => r.items.map(i => i.name))).toEqual([['fastfwd', 'app_segment']]);
  });

  it('pairs by id when a millisecond tie would cross two segments starting together', () => {
    const rows = pack(
      withNodes([
        node([
          // Two execution items and two segments share their start moments. The equal-id match pairs
          // #2 with Segment #2 and #3 with Segment #3 rather than crossing.
          [0, 56, 227, { id: 2 }],
          [0, 56, 227, { id: 3 }],
          [2, 283, 623, { id: 2 }],
          [2, 283, 592, { id: 3 }],
        ]),
      ])
    );
    const merged = rows.filter(r => r.items.length === 2).map(r => r.items.map(i => [i.name, i.id]));
    expect(merged).toEqual([
      [
        ['execution', 2],
        ['app_segment', 2],
      ],
      [
        ['execution', 3],
        ['app_segment', 3],
      ],
    ]);
  });

  it('keeps an id-less execution item on its own row', () => {
    // Without an id the execution item cannot claim a segment, so it stays its own row.
    const rows = pack(withNodes([node([[0, 100, 50], [2, 150, 200, { id: 0 }]])]));
    expect(rows.map(r => r.items.map(i => i.name))).toEqual([['execution'], ['app_segment']]);
  });

  it('keeps an execution item on its own row when no segment shares its id', () => {
    const rows = pack(
      withNodes([
        // The execution id 0 has no segment on its own node.
        node([[0, 100, 50, { id: 0 }]]),
        // A segment with id 0 sits on another node and must not claim the first node's execution item.
        node([[2, 150, 100, { id: 0 }]]),
      ])
    );
    expect(rows.map(r => r.items.map(i => i.name))).toEqual([['execution'], ['app_segment']]);
  });

  it('folds only execution and fast-forward, never another kind sharing a segment id', () => {
    const rows = pack(
      withNodes([
        // A leaf carrying id 0 shares the segment index space but is not a lead-in, so it keeps its own
        // row and only the execution item folds onto Segment #0.
        node([[3, 100, 50, { id: 0 }], [0, 120, 40, { id: 0 }], [2, 160, 100, { id: 0 }]]),
      ])
    );
    expect(rows.map(r => r.items.map(i => i.name))).toEqual([['leaf'], ['execution', 'app_segment']]);
  });

  it('orders merged and lone rows together by earliest start', () => {
    const rows = pack(
      withNodes([
        node([
          // A merged pair whose execution item starts at 500ms.
          [0, 500, 100, { id: 0 }],
          [2, 600, 100, { id: 0 }],
          // A lone segment earlier at 200ms and a leaf earlier still at 50ms.
          [2, 200, 100, { id: 16 }],
          [3, 50, 10],
        ]),
      ])
    );
    expect(rows.map(r => [r.startSec, r.items.map(i => i.name)])).toEqual([
      [0.05, ['leaf']],
      [0.2, ['app_segment']],
      [0.5, ['execution', 'app_segment']],
    ]);
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

// An openvm document whose pipeline carries the six base kinds and the fourteen sub-step component
// kinds, plus the sub-phase template that tints the components.
const COMPONENT_BASE_KINDS: PipelineStep[] = [
  { name: 'execution', label: 'Metered Execution', phase: 'execution', paired: false },
  { name: 'fastfwd', label: 'Fast Forward', phase: 'execution', paired: false },
  { name: 'app_segment', label: 'Segment', phase: 'segment', paired: false },
  { name: 'leaf', label: 'Recursion', phase: 'recursion', paired: false },
  { name: 'internal', label: 'Internal Aggregation', phase: 'recursion', paired: false },
  { name: 'wrap', label: 'Wrap', phase: 'wrap', paired: false },
];
const componentBench: Benchmark = (() => {
  const base = openvmBenchmark([], true);
  const pipeline: PipelineStep[] = [
    ...COMPONENT_BASE_KINDS,
    ...openvmSubPhases.map(sub => ({ name: sub.name, label: sub.label, phase: sub.phase, paired: false })),
  ];
  return { ...base, software: { ...base.software, zkvm: { ...base.software.zkvm, pipeline } } };
})();
const componentRegistry = buildPhaseRegistry(componentBench);

describe('decodePipeline sub-step components', () => {
  it('colors a component by its sub-phase tint and reads its group', () => {
    const block = withNodes([
      node([
        [6, 100, 5, { id: 4, group: 0 }],
        [7, 105, 620, { id: 4, group: 0 }],
        [0, 50, 40, { id: 4 }],
      ]),
    ]);
    const model = decodePipeline(componentBench, block, nodes, componentRegistry);
    const preflight = model.items.find(i => i.kind === 6)!;
    expect(preflight.label).toBe('execute_preflight');
    expect(preflight.phase).toBe('segment');
    expect([preflight.id, preflight.group]).toEqual([4, 0]);
    // The component colors by its sub-phase tint, the same the block-level split chart uses, not the
    // flat owner color the monolithic segment bar would take.
    expect(preflight.color).toBe(componentRegistry.color(subPhaseKey('segment', 'execute_preflight')));
    expect(preflight.color).not.toBe(componentRegistry.color('segment'));
    // A base kind keeps its flat phase color and carries no group.
    const execution = model.items.find(i => i.kind === 0)!;
    expect(execution.color).toBe(componentRegistry.color('execution'));
    expect(execution.group).toBeUndefined();
  });
});

describe('packRows sub-step components', () => {
  it('packs the components of one group onto a single row in start order', () => {
    const block = withNodes([
      node([
        [8, 725, 240, { id: 4, group: 0 }],
        [6, 100, 5, { id: 4, group: 0 }],
        [7, 105, 620, { id: 4, group: 0 }],
        [13, 2000, 3, { group: 1 }],
        [14, 2003, 97, { group: 1 }],
      ]),
    ]);
    const rows = packRows(decodePipeline(componentBench, block, nodes, componentRegistry).items);
    // One row per group, its components in start order, the groups ordered by earliest start.
    expect(rows.map(r => r.items.map(i => i.kind))).toEqual([
      [6, 7, 8],
      [13, 14],
    ]);
    expect(rows[0]!.items.every(i => i.group === 0)).toBe(true);
    expect(rows[1]!.items.every(i => i.group === 1)).toBe(true);
  });

  it('keeps a separate group on its own row even when its components share a kind and start', () => {
    // Two proofs whose components share the same kind and start moment stay apart by group, so a
    // leaf and an internal of the same recursion slot never read as one proof.
    const block = withNodes([
      node([
        [13, 100, 5, { group: 0 }],
        [13, 100, 5, { group: 1 }],
      ]),
    ]);
    const rows = packRows(decodePipeline(componentBench, block, nodes, componentRegistry).items);
    expect(rows.map(r => r.items.map(i => i.group))).toEqual([[0], [1]]);
  });

  it('folds a segment execution and fast-forward onto its component run on one row', () => {
    // The row is composed as the monolithic segment was, the Metered Execution lead and the Fast
    // Forward folding onto the app-segment component run by the shared segment id, while a leaf
    // component run keeps its own row.
    const block = withNodes([
      node([
        [0, 100, 50, { id: 4 }],
        [1, 150, 5, { id: 4 }],
        [6, 155, 5, { id: 4, group: 0 }],
        [7, 160, 20, { id: 4, group: 0 }],
        [8, 180, 30, { id: 4, group: 0 }],
        [13, 2000, 3, { group: 1 }],
        [14, 2003, 97, { group: 1 }],
      ]),
    ]);
    const rows = packRows(decodePipeline(componentBench, block, nodes, componentRegistry).items);
    expect(rows.map(r => r.items.map(i => i.name))).toEqual([
      ['execution', 'fastfwd', 'execute_preflight', 'trace_gen', 'main_trace_commit'],
      ['execute_preflight', 'trace_gen'],
    ]);
    // The merged segment row takes the execution start, the leaf run its own.
    expect(rows[0]!.startSec).toBe(0.1);
    expect(rows[1]!.startSec).toBe(2);
  });
});
