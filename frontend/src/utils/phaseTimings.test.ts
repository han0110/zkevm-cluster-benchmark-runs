import { describe, it, expect } from 'vitest';
import { fixture } from '@/test/fixture';
import { openvmBenchmark, openvmBlock, openvmNode, win } from '@/test/openvmFixture';
import { buildPhaseRegistry } from '@/utils/phases';
import { apportionPercents, hasOverlapPhases, meanWindowPhases, placedSegmentPieces, splitOverlap, traceLabelBaseSec } from '@/utils/phaseTimings';
import type { PhaseMean, Span } from '@/utils/phaseTimings';
import type { Block } from '@/types/benchmark';

// Registry for exactly the given blocks, so each case shapes its own windows.
const registryOf = (blocks: Block[]) => buildPhaseRegistry(openvmBenchmark(blocks));

// One block whose cluster windows on node A are input [0, 0.2], metered execution [0.4, 1.4], segment
// [1.2, 3.0], recursion [2.4, 3.6], and wrap [3.6, 4.0] in seconds, and on node B input [0, 0.3], metered
// execution [0.4, 1.6], segment [1.3, 3.4], recursion [2.8, 3.8], and no wrap. Metered overlaps segment,
// segment overlaps recursion, and metered stays clear of recursion, so the chain carries two bands and no
// triple region. Node A works to 4.0 s and node B to 3.8 s.
const nodeA = openvmNode([win(0, 200), win(400, 1000), win(1200, 1800), win(2400, 1200), win(3600, 400)]);
const nodeB = openvmNode([win(0, 300), win(400, 1200), win(1300, 2100), win(2800, 1000), null]);
const overlappingBlock = openvmBlock('b0', [nodeA, nodeB]);

describe('hasOverlapPhases', () => {
  it('flags the overlap preset and not the zisk fixture', () => {
    expect(hasOverlapPhases(buildPhaseRegistry(fixture))).toBe(false);
    expect(hasOverlapPhases(registryOf([overlappingBlock]))).toBe(true);
  });
});

describe('meanWindowPhases cluster total mode', () => {
  it('divides each phase sum by the counted block count over the pooled-time shares', () => {
    // Two identical blocks of two node data points working 4.0 s and 3.8 s. Phase sums over the 15.6 s of
    // working time give input 1.0, metered 4.4, segment 7.8, recursion 4.4, wrap 0.8, and a Rest of 0.6.
    // Cluster total divides each by the two counted blocks, so the seconds read the summed node-time per
    // block, half of what dividing by the four data points would give.
    const blocks = [overlappingBlock, { ...overlappingBlock, name: 'b1' }];
    const { phases, total } = meanWindowPhases(blocks, registryOf(blocks), 'clusterTotal');
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(total).toBeCloseTo(7.8);
    expect(byKey['input']!.seconds).toBeCloseTo(0.5);
    expect(byKey['metered_execution']!.seconds).toBeCloseTo(2.2);
    expect(byKey['segment']!.seconds).toBeCloseTo(3.9);
    expect(byKey['recursion']!.seconds).toBeCloseTo(2.2);
    expect(byKey['wrap']!.seconds).toBeCloseTo(0.4);
    expect(byKey['rest']!.seconds).toBeCloseTo(0.3);
    // Shares divide each sum by the summed working span, unchanged by the block-count seconds divisor.
    expect(byKey['input']!.placed!.frac).toBeCloseTo(0.0641);
    expect(byKey['metered_execution']!.placed!.frac).toBeCloseTo(0.2821);
    expect(byKey['segment']!.placed!.frac).toBeCloseTo(0.5);
    expect(byKey['recursion']!.placed!.frac).toBeCloseTo(0.2821);
    expect(byKey['wrap']!.placed!.frac).toBeCloseTo(0.0513);
    expect(byKey['rest']!.placed!.frac).toBeCloseTo(0.0385);
  });

  it('weighs a node by its working span so a longer node shifts the run share', () => {
    // A node working 2 s that spends 1 s in segment (0.5) and one working 10 s that spends 8 s (0.8). The
    // sum-based share 9 s over 12 s is 0.75, weighted toward the longer node, not the 0.65 the mean of the
    // two node ratios would give.
    const shortBlock = openvmBlock('short', [openvmNode([win(0, 100), win(100, 100), win(200, 1000), win(1000, 800), win(1800, 200)])]);
    const longBlock = openvmBlock('long', [openvmNode([win(0, 200), win(200, 300), win(500, 8000), win(8000, 1500), win(9500, 500)])]);
    const blocks = [shortBlock, longBlock];
    const { phases } = meanWindowPhases(blocks, registryOf(blocks), 'clusterTotal');
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(byKey['segment']!.placed!.frac).toBeCloseTo(0.75);
  });

  it('tiles the chain sequentially with two bands and Rest closing the row', () => {
    const blocks = [overlappingBlock];
    const { phases } = meanWindowPhases(blocks, registryOf(blocks), 'clusterTotal');
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    // Input then metered tile end to end, then segment starts the first band's width before metered ends.
    expect(byKey['input']!.placed!.start).toBeCloseTo(0);
    expect(byKey['metered_execution']!.placed!.start).toBeCloseTo(0.0641);
    expect(byKey['segment']!.placed!.start).toBeCloseTo(0.2821);
    expect(byKey['segment']!.placed!.width).toBeCloseTo(0.5);
    // Recursion starts the second band's width before segment ends, so segment joins both bands.
    expect(byKey['recursion']!.placed!.start).toBeCloseTo(0.6282);
    expect(byKey['recursion']!.placed!.width).toBeCloseTo(0.2821);
    // Wrap follows the chain and Rest fills the right end so the row closes at exactly one.
    expect(byKey['wrap']!.placed!.start).toBeCloseTo(0.9103);
    expect(byKey['rest']!.placed!.start).toBeCloseTo(0.9615);
    expect(byKey['rest']!.placed!.start + byKey['rest']!.placed!.width).toBeCloseTo(1, 10);
  });
});

describe('meanWindowPhases per node mode', () => {
  it('divides each phase by the node data points carrying it so a single-node phase keeps its full mean', () => {
    // Segment runs on both nodes, so its 3.9 s over two data points reads 1.95 s. Wrap runs on the one
    // aggregator node, so its 0.4 s keeps the full single-node duration rather than the 0.2 s dividing by
    // both data points would give. Per node is the default mode.
    const blocks = [overlappingBlock];
    const { phases, total } = meanWindowPhases(blocks, registryOf(blocks));
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(total).toBeCloseTo(3.9);
    expect(byKey['input']!.seconds).toBeCloseTo(0.25);
    expect(byKey['metered_execution']!.seconds).toBeCloseTo(1.1);
    expect(byKey['segment']!.seconds).toBeCloseTo(1.95);
    expect(byKey['recursion']!.seconds).toBeCloseTo(1.1);
    expect(byKey['wrap']!.seconds).toBeCloseTo(0.4);
    expect(byKey['rest']!.seconds).toBeCloseTo(0.15);
  });

  it('renormalizes the shares and tiles the chain from the mode seconds, closing it at exactly one', () => {
    // The mode seconds sum to 4.8 s, and with the 0.25 s and 0.6 s bands subtracted once the row normalizes
    // by 4.1 s. Wrap's full per-node duration lifts its share to 0.0976, near twice its 0.0513 pooled-time
    // share, while segment keeps 0.4756.
    const blocks = [overlappingBlock];
    const { phases } = meanWindowPhases(blocks, registryOf(blocks), 'perNode');
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(byKey['input']!.placed!.frac).toBeCloseTo(0.061);
    expect(byKey['metered_execution']!.placed!.frac).toBeCloseTo(0.2683);
    expect(byKey['segment']!.placed!.frac).toBeCloseTo(0.4756);
    expect(byKey['recursion']!.placed!.frac).toBeCloseTo(0.2683);
    expect(byKey['wrap']!.placed!.frac).toBeCloseTo(0.0976);
    expect(byKey['rest']!.placed!.frac).toBeCloseTo(0.0366);
    // Input then metered tile end to end, segment shares the first band, recursion the second.
    expect(byKey['input']!.placed!.start).toBeCloseTo(0);
    expect(byKey['metered_execution']!.placed!.start).toBeCloseTo(0.061);
    expect(byKey['segment']!.placed!.start).toBeCloseTo(0.2683);
    expect(byKey['recursion']!.placed!.start).toBeCloseTo(0.5976);
    expect(byKey['wrap']!.placed!.start).toBeCloseTo(0.8659);
    expect(byKey['rest']!.placed!.start + byKey['rest']!.placed!.width).toBeCloseTo(1, 10);
  });

  it('charges no Rest for the trailing idle after a node finishes early', () => {
    // One node works to 5 s and another finishes at 3 s, both fully covered by their phases. The early
    // node's 2 s of idle while the first keeps working is not Rest, so the run carries none and its span
    // averages 4 s, not the 5 s wall clock. Metered clears segment on both nodes, leaving only the
    // segment/recursion band.
    const late = openvmNode([win(0, 500), win(500, 500), win(1000, 2000), win(2500, 1500), win(4000, 1000)]);
    const early = openvmNode([win(0, 500), win(500, 500), win(1000, 1500), win(2000, 1000), null]);
    const block = openvmBlock('b0', [late, early]);
    const { phases, total } = meanWindowPhases([block], registryOf([block]));
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(total).toBeCloseTo(4);
    expect(byKey['rest']!.seconds).toBeCloseTo(0);
    expect(byKey['rest']!.placed!.frac).toBeCloseTo(0);
    expect(byKey['segment']!.placed!.frac).toBeCloseTo(0.3889);
  });

  it('closes a single-block bucket to exactly one', () => {
    const { phases } = meanWindowPhases([overlappingBlock], registryOf([overlappingBlock]));
    const rest = phases.find(p => p.key === 'rest')!;
    expect(rest.placed!.start + rest.placed!.width).toBeCloseTo(1, 10);
    expect(rest.hatched).toBe(true);
  });

  it('skips a node without data and a node that did not participate', () => {
    // Two ghost nodes ride alongside the two real ones, one recording nothing and one flagged as not
    // participating, so both drop out and the shares match the two-node block.
    const ghosts = openvmBlock('b0', [
      nodeA,
      nodeB,
      openvmNode([null, null, null, null, null]),
      { ...openvmNode([win(0, 200), win(400, 1000), win(1200, 1800), win(2400, 1200), win(3600, 400)]), participated: false },
    ]);
    const { phases, total } = meanWindowPhases([ghosts], registryOf([ghosts]));
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    expect(total).toBeCloseTo(3.9);
    expect(byKey['segment']!.placed!.frac).toBeCloseTo(0.4756);
    expect(byKey['segment']!.seconds).toBeCloseTo(1.95);
    expect(byKey['rest']!.placed!.frac).toBeCloseTo(0.0366);
  });
});

describe('meanWindowPhases chain bands', () => {
  it('tiles the row to one when every overlap pair is disjoint', () => {
    // Metered [0.1, 0.4] ends before segment [0.6, 1.0] starts, and segment ends before recursion
    // [2.0, 3.0] starts, so neither pair carries an overlap and no band is subtracted.
    const disjoint = openvmBlock('b0', [openvmNode([win(0, 100), win(100, 300), win(600, 400), win(2000, 1000), win(3000, 500)])]);
    const { phases } = meanWindowPhases([disjoint], registryOf([disjoint]));
    const rest = phases.find(p => p.key === 'rest')!;
    // With no band subtracted the phases still tile the row to exactly one.
    expect(rest.placed!.start + rest.placed!.width).toBeCloseTo(1, 10);
  });
});

describe('splitOverlap', () => {
  it('splits two overlapping spans into first-only, intersection, and second-only parts', () => {
    expect(splitOverlap({ start: 0, end: 3 }, { start: 2, end: 5 })).toEqual({
      firstOnly: [{ start: 0, end: 2 }],
      intersection: { start: 2, end: 3 },
      secondOnly: [{ start: 3, end: 5 }],
    });
  });

  it('keeps disjoint spans whole with a null intersection', () => {
    expect(splitOverlap({ start: 0, end: 1 }, { start: 2, end: 3 })).toEqual({
      firstOnly: [{ start: 0, end: 1 }],
      intersection: null,
      secondOnly: [{ start: 2, end: 3 }],
    });
  });

  it('leaves remainder parts on both sides of a contained span', () => {
    expect(splitOverlap({ start: 0, end: 10 }, { start: 2, end: 4 })).toEqual({
      firstOnly: [{ start: 0, end: 2 }, { start: 4, end: 10 }],
      intersection: { start: 2, end: 4 },
      secondOnly: [],
    });
  });

  it('treats identical spans as pure intersection', () => {
    expect(splitOverlap({ start: 1, end: 2 }, { start: 1, end: 2 })).toEqual({
      firstOnly: [],
      intersection: { start: 1, end: 2 },
      secondOnly: [],
    });
  });
});

describe('placedSegmentPieces', () => {
  it('keeps the uncovered parts solid and stripes the intersection against the earlier sibling', () => {
    const segments = [{ start: 0, end: 3 }, { start: 2, end: 5 }];
    expect(placedSegmentPieces(segments, 0)).toEqual({ solid: [{ start: 0, end: 2 }], striped: [] });
    expect(placedSegmentPieces(segments, 1)).toEqual({
      solid: [{ start: 3, end: 5 }],
      striped: [{ span: { start: 2, end: 3 }, first: segments[0] }],
    });
  });

  it('splits an earlier phase containing a later sibling into two solid slivers', () => {
    const segments = [{ start: 0, end: 10 }, { start: 2, end: 4 }];
    expect(placedSegmentPieces(segments, 0)).toEqual({ solid: [{ start: 0, end: 2 }, { start: 4, end: 10 }], striped: [] });
    expect(placedSegmentPieces(segments, 1)).toEqual({
      solid: [],
      striped: [{ span: { start: 2, end: 4 }, first: segments[0] }],
    });
  });

  it('splits a later phase containing an earlier sibling into striped and two solid slivers', () => {
    const segments = [{ start: 2, end: 4 }, { start: 0, end: 10 }];
    expect(placedSegmentPieces(segments, 1)).toEqual({
      solid: [{ start: 0, end: 2 }, { start: 4, end: 10 }],
      striped: [{ span: { start: 2, end: 4 }, first: segments[0] }],
    });
  });

  it('leaves disjoint siblings whole with nothing striped', () => {
    const segments = [{ start: 0, end: 1 }, { start: 2, end: 3 }];
    expect(placedSegmentPieces(segments, 0)).toEqual({ solid: [{ start: 0, end: 1 }], striped: [] });
    expect(placedSegmentPieces(segments, 1)).toEqual({ solid: [{ start: 2, end: 3 }], striped: [] });
  });

  it('stripes a triple overlap for its two latest phases and never repaints an interval', () => {
    // Three chained spans, metered [0, 6], segment [3, 9], recursion [5, 12], overlap so all three cover
    // [5, 6]. That triple interval paints once as the later segment/recursion pair, the earlier
    // metered/segment band stops where recursion begins, and no metered/recursion stripe is drawn.
    const segments = [{ start: 0, end: 6 }, { start: 3, end: 9 }, { start: 5, end: 12 }];
    expect(placedSegmentPieces(segments, 0)).toEqual({ solid: [{ start: 0, end: 3 }], striped: [] });
    expect(placedSegmentPieces(segments, 1)).toEqual({
      solid: [],
      striped: [{ span: { start: 3, end: 5 }, first: segments[0] }],
    });
    expect(placedSegmentPieces(segments, 2)).toEqual({
      solid: [{ start: 9, end: 12 }],
      striped: [{ span: { start: 5, end: 9 }, first: segments[1] }],
    });
  });
});

describe('overview phase breakdown pieces', () => {
  // The rendered pieces of a breakdown row split the way StackedPhaseBars does, in left-to-right order,
  // each a phase-only solid remainder or a striped band named for both phases. This is the same
  // placedSegmentPieces primitive the trace and the overview label paths draw from.
  const rowPieces = (phases: PhaseMean[]): { name: string; width: number }[] => {
    const placed = phases
      .filter(p => p.placed && p.placed.width > 0)
      .map(p => ({ key: p.key, label: p.label, start: p.placed!.start, end: p.placed!.start + p.placed!.width }));
    const width = (s: Span): number => s.end - s.start;
    return placed.flatMap((seg, i) => {
      const { solid, striped } = placedSegmentPieces(placed, i);
      return [
        ...striped.map(piece => ({ name: `${piece.first.label} + ${seg.label}`, width: width(piece.span) })),
        { name: seg.label, width: solid.reduce((sum, s) => sum + width(s), 0) },
      ];
    });
  };

  it('sums every piece to exactly one in mean and sum modes', () => {
    for (const mode of ['perNode', 'clusterTotal'] as const) {
      const { phases } = meanWindowPhases([overlappingBlock], registryOf([overlappingBlock]), mode);
      const sum = rowPieces(phases).reduce((total, piece) => total + piece.width, 0);
      expect(sum).toBeCloseTo(1, 10);
    }
  });

  it('labels a solid piece with its phase share less its own bands, not the full phase share', () => {
    const { phases } = meanWindowPhases([overlappingBlock], registryOf([overlappingBlock]));
    const byKey = Object.fromEntries(phases.map(p => [p.key, p]));
    const pieces = rowPieces(phases);
    const meteredSolid = pieces.find(p => p.name === 'Metered Execution')!;
    const meteredBand = pieces.find(p => p.name === 'Metered Execution + Segment')!;
    // The metered solid piece is the metered phase share less the metered/segment band it shares, well
    // under its full phase share.
    expect(meteredSolid.width).toBeCloseTo(byKey['metered_execution']!.placed!.frac - meteredBand.width, 10);
    expect(meteredSolid.width).toBeLessThan(byKey['metered_execution']!.placed!.frac);
  });

  it('apportions the row pieces so the label integers sum to exactly 100', () => {
    const { phases } = meanWindowPhases([overlappingBlock], registryOf([overlappingBlock]));
    const percents = apportionPercents(rowPieces(phases).map(p => p.width));
    expect(percents.reduce((sum, v) => sum + v, 0)).toBe(100);
  });

  it('orders the pieces left to right with each band between its two phases', () => {
    const { phases } = meanWindowPhases([overlappingBlock], registryOf([overlappingBlock]));
    expect(rowPieces(phases).map(p => p.name)).toEqual([
      'Input Transfer',
      'Metered Execution',
      'Metered Execution + Segment',
      'Segment',
      'Segment + Recursion',
      'Recursion',
      'Wrap',
      'Rest',
    ]);
  });
});

describe('traceLabelBaseSec', () => {
  it('divides the trace percents by the reported proving time on an overlap preset only', () => {
    expect(traceLabelBaseSec(overlappingBlock, registryOf([overlappingBlock]))).toBeCloseTo(4);
    expect(traceLabelBaseSec(fixture.runs[0]!.blocks[0]!, buildPhaseRegistry(fixture))).toBeUndefined();
  });

  it('marks a block reporting no proving time with a null base', () => {
    const crashed: Block = { ...overlappingBlock, status: 'crashed', proving_ms: null };
    expect(traceLabelBaseSec(crashed, registryOf([overlappingBlock]))).toBeNull();
  });
});

describe('apportionPercents', () => {
  it('rounds a set that naively totals 101 back to 100 by largest remainder', () => {
    // Independent rounding of 33.5, 33.5, 33 gives 34 + 34 + 33 = 101. Hamilton floors to 99 then adds the
    // single deficit point to the first largest remainder, closing the row at 100.
    const out = apportionPercents([0.335, 0.335, 0.33]);
    expect(out).toEqual([34, 33, 33]);
    expect(out.reduce((sum, v) => sum + v, 0)).toBe(100);
  });

  it('keeps a zero entry at zero and still sums to 100', () => {
    const out = apportionPercents([0.5, 0.5, 0]);
    expect(out).toEqual([50, 50, 0]);
    expect(out.reduce((sum, v) => sum + v, 0)).toBe(100);
  });

  it('sums to the rounded coverage when the fractions cover less than the whole', () => {
    // Trace pieces cover only part of the proving time, so the percents honestly total that coverage
    // rather than being inflated to 100.
    expect(apportionPercents([0.4, 0.3]).reduce((sum, v) => sum + v, 0)).toBe(70);
  });

  it('returns all zeros for a zero-sum input', () => {
    expect(apportionPercents([0, 0, 0])).toEqual([0, 0, 0]);
  });
});
