/*
 * Per-block proving-phase durations for the phase-timing charts, keyed by phase name so any zkVM preset
 * works. A node view reports its own per-phase times. The cluster view reports the critical path where a
 * phase cost is the gap between consecutive cluster phase ends, each ending when the last node finishes.
 */

import type { PhaseEntry, PhaseRegistry } from '@/utils/phases';
import { blockLabel } from '@/utils/phases';
import { msToSec } from '@/utils/format';
import type { Block, PhaseWindow } from '@/types/benchmark';

export interface PhaseSeries {
  // X-axis labels, one per block.
  labels: string[];
  // Per-block durations in seconds, keyed by phase name.
  values: Record<string, number[]>;
  // Total proving seconds per block.
  total: number[];
  // Source blocks aligned with labels, so a caller can map a label index to its block.
  blocks: Block[];
}

// Returns an empty per-phase-name accumulator for the registry's phases.
const emptyValues = (registry: PhaseRegistry): Record<string, number[]> =>
  Object.fromEntries(registry.list.map(p => [p.name, []]));

// Seconds-since-block-start (start, end) of a phase window, or null when absent. The single window the
// Gantt rows, per-proof telemetry bands, and cluster critical path all rebase from.
export const windowSeconds = (win: PhaseWindow | null): { start: number; end: number } | null =>
  win ? { start: msToSec(win.start_ms), end: msToSec(win.start_ms + win.dur_ms) } : null;

// Seconds-since-block-start of the end of a window, or null when the window is absent.
const endSec = (win: PhaseWindow | null): number | null => windowSeconds(win)?.end ?? null;

// Whether a block has anything to plot on its trace, a phase window on some node or a crash marker. A
// block that ended before any node reported progress has neither, so it carries no timeline.
export function hasTimeline(block: Block): boolean {
  return block.nodes.some(node => node.crashed_ms != null || node.phases.some(p => p != null));
}

// Per-block phase durations for one node from its own phase windows. The aggregate phase is non-zero
// only on the blocks the node aggregated.
export function nodePhaseSeries(blocks: Block[], nodeIndex: number, registry: PhaseRegistry): PhaseSeries {
  const labels: string[] = [];
  const values = emptyValues(registry);
  const total: number[] = [];
  const out: Block[] = [];
  for (const block of blocks) {
    const node = block.nodes[nodeIndex];
    if (!node) continue;
    labels.push(blockLabel(block));
    out.push(block);
    for (const phase of registry.list) {
      const win = node.phases[phase.index] ?? null;
      values[phase.name]?.push(win ? msToSec(win.dur_ms) : 0);
    }
    // A crashed proof carries no whole-proof time, so its total envelope reads as zero.
    total.push(block.proving_ms != null ? msToSec(block.proving_ms) : 0);
  }
  return { labels, values, total, blocks: out };
}

// Per-block cluster critical-path phase costs. Each phase ends when the last node finishes it, so a
// phase cost is the gap from the previous phase's cluster end, reckoned from the block start.
export function clusterPhaseSeries(blocks: Block[], registry: PhaseRegistry): PhaseSeries {
  const labels: string[] = [];
  const values = emptyValues(registry);
  const total: number[] = [];
  const out: Block[] = [];
  for (const block of blocks) {
    if (block.status !== 'success' || block.nodes.length === 0) continue;
    labels.push(blockLabel(block));
    out.push(block);

    let prevEnd = 0;
    for (const phase of registry.list) {
      const ends = block.nodes
        .map(n => endSec(n.phases[phase.index] ?? null))
        .filter((v): v is number => v != null);
      const end = ends.length ? Math.max(...ends) : prevEnd;
      values[phase.name]?.push(Math.max(0, end - prevEnd));
      prevEnd = Math.max(prevEnd, end);
    }
    total.push(prevEnd);
  }
  return { labels, values, total, blocks: out };
}

// Absolute placement of a full-height phase bar on the unit-width breakdown row. `frac` is the share
// the labels report, the sum-based phase share, independent of the rendered width.
export interface PhasePlacement {
  start: number;
  width: number;
  frac: number;
}

// Half-open interval on a shared axis, a phase span in seconds or in row fractions.
export interface Span {
  start: number;
  end: number;
}

// Splits two spans into the parts covered only by the first, their shared intersection, and the parts
// covered only by the second. Disjoint spans yield a null intersection, and a span containing the other
// leaves remainder parts on both of its sides. Empty parts are dropped.
export function splitOverlap(
  first: Span,
  second: Span
): { firstOnly: Span[]; intersection: Span | null; secondOnly: Span[] } {
  const start = Math.max(first.start, second.start);
  const end = Math.min(first.end, second.end);
  const nonEmpty = (spans: Span[]): Span[] => spans.filter(span => span.end > span.start);
  if (start >= end) return { firstOnly: nonEmpty([first]), intersection: null, secondOnly: nonEmpty([second]) };
  return {
    firstOnly: nonEmpty([{ start: first.start, end: start }, { start: end, end: first.end }]),
    intersection: { start, end },
    secondOnly: nonEmpty([{ start: second.start, end: start }, { start: end, end: second.end }]),
  };
}

// One placed segment of a row split against its span siblings. Solid pieces are the parts no sibling
// covers. Each striped piece pairs an overlapped interval with the nearest earlier sibling covering it,
// while a later sibling claims any interval it also covers, so an interval two or more phases share is
// striped for its two latest phases and the whole row paints each interval exactly once, with no paint
// order left to chance across a triple overlap.
export function placedSegmentPieces<T extends Span>(
  segments: T[],
  index: number
): { solid: Span[]; striped: { span: Span; first: T }[] } {
  const segment = segments[index]!;
  const subtract = (pieces: Span[], sibling: Span): Span[] =>
    pieces.flatMap(piece => splitOverlap(piece, sibling).firstOnly);
  // The region this segment paints, itself less every later sibling. A later phase paints any interval
  // it shares, so in a chain the latest phase present owns a multi-phase overlap.
  const owned = segments.slice(index + 1).reduce<Span[]>(subtract, [{ start: segment.start, end: segment.end }]);
  // Solid where no sibling at all covers the owned region.
  const solid = segments.slice(0, index).reduce<Span[]>(subtract, owned);
  // The owned region an earlier sibling also covers is striped against the nearest such sibling, walking
  // closest to farthest so a triple overlap stripes for its two latest phases rather than once per pair.
  const striped: { span: Span; first: T }[] = [];
  let remaining = owned;
  for (let earlier = index - 1; earlier >= 0; earlier--) {
    const sibling = segments[earlier]!;
    for (const piece of remaining) {
      const shared = splitOverlap(piece, sibling).intersection;
      if (shared) striped.push({ span: shared, first: sibling });
    }
    remaining = subtract(remaining, sibling);
  }
  return { solid, striped };
}

// Largest-remainder (Hamilton) apportionment of fractions to whole percents. Each fraction floors to its
// percent, then the points left to the input's rounded total go one at a time to the largest fractional
// remainders. Fractions summing to one yield percents summing to 100, while a set covering less of the
// whole sums to that rounded coverage. A zero-sum input returns all zeros.
export function apportionPercents(fractions: number[]): number[] {
  const total = fractions.reduce((sum, f) => sum + f, 0);
  if (total <= 0) return fractions.map(() => 0);
  const scaled = fractions.map(f => f * 100);
  const result = scaled.map(Math.floor);
  const deficit = Math.round(total * 100) - result.reduce((sum, v) => sum + v, 0);
  const byRemainder = scaled
    .map((v, i) => ({ i, remainder: v - Math.floor(v) }))
    .sort((a, b) => b.remainder - a.remainder);
  for (let k = 0; k < deficit && k < byRemainder.length; k++) result[byRemainder[k]!.i]! += 1;
  return result;
}

export interface PhaseMean {
  key: string;
  label: string;
  seconds: number;
  // Present only for an overlap preset, where the breakdown row is placed not stacked.
  placed?: PhasePlacement;
  // Marks the Rest filler that closes an overlap-preset row, drawn as a hatched band the axis tooltip
  // still lists.
  hatched?: boolean;
}

// Whether the registry carries any overlap-flagged phase, the switch to window-ratio breakdown math.
export const hasOverlapPhases = (registry: PhaseRegistry): boolean => registry.list.some(p => p.overlap);

// Percent denominator for the block-trace labels. An overlap preset labels every trace piece against
// the block's reported proving time, null when the block reports none so those labels read in
// seconds. A preset without overlap keeps the stacked-row shares, marked by undefined.
export const traceLabelBaseSec = (block: Block, registry: PhaseRegistry): number | null | undefined =>
  hasOverlapPhases(registry) ? (block.proving_ms != null ? msToSec(block.proving_ms) : null) : undefined;

// Mean cluster critical-path cost of each phase across all successful blocks.
export function meanClusterPhases(blocks: Block[], registry: PhaseRegistry): { phases: PhaseMean[]; total: number } {
  const series = clusterPhaseSeries(blocks, registry);
  const n = series.labels.length || 1;
  const mean = (arr: number[]): number => arr.reduce((sum, v) => sum + v, 0) / n;
  const phases: PhaseMean[] = registry.list.map(phase => ({
    key: phase.name,
    label: phase.label,
    seconds: mean(series.values[phase.name] ?? []),
  }));
  const total = phases.reduce((sum, p) => sum + p.seconds, 0) || 1;
  return { phases, total };
}

// Total time the union of a node's phase windows covers, merging overlaps so concurrent windows count
// once. The seconds a node's own working span leaves uncovered is its Rest.
function unionLength(spans: Span[]): number {
  const sorted = [...spans].sort((a, b) => a.start - b.start);
  let total = 0;
  let prevEnd = -Infinity;
  for (const span of sorted) {
    const start = Math.max(span.start, prevEnd);
    if (span.end > start) total += span.end - start;
    prevEnd = Math.max(prevEnd, span.end);
  }
  return total;
}

// Consecutive overlap pairs from the ordered overlap-phase indices, one striped band per adjacent pair.
// Two overlap phases give one pair, three give two, and so on, each phase but the endpoints joining two.
const consecutivePairs = (overlapIndices: number[]): [number, number][] =>
  overlapIndices.slice(1).map((index, k) => [overlapIndices[k]!, index]);

// Measured overlap seconds of one pair's two windows, zero when either is absent or they are disjoint.
const pairOverlapSeconds = (first: Span | null, second: Span | null): number =>
  first && second ? Math.max(0, Math.min(first.end, second.end) - Math.max(first.start, second.start)) : 0;

// Lays the phase shares out sequentially on the unit row, returning each phase's placement and the
// position the sequence ends at, which Rest fills. Non-overlap phases tile end to end. The overlap phases
// form a chain, each after the first pulling back from the previous overlap phase's end by its pair's
// band, so consecutive overlap phases share a striped band the width of that pair's overlap share.
function placeSumPhases(
  list: PhaseEntry[],
  pct: number[],
  overlapIndices: number[],
  bandPct: number[]
): { placements: PhasePlacement[]; end: number } {
  // The band each overlap phase past the first pulls back by, keyed by that phase's own index.
  const pullBack = new Map<number, number>();
  consecutivePairs(overlapIndices).forEach(([, second], k) => pullBack.set(second, bandPct[k] ?? 0));
  const placements = list.map(() => ({ start: 0, width: 0, frac: 0 }));
  let pos = 0;
  let prevOverlapEnd = 0;
  list.forEach((_, i) => {
    if (overlapIndices.includes(i)) {
      const start = pullBack.has(i) ? Math.max(0, prevOverlapEnd - pullBack.get(i)!) : pos;
      placements[i] = { start, width: pct[i]!, frac: pct[i]! };
      prevOverlapEnd = start + pct[i]!;
      pos = Math.max(pos, prevOverlapEnd);
    } else {
      placements[i] = { start: pos, width: pct[i]!, frac: pct[i]! };
      pos += pct[i]!;
    }
  });
  return { placements, end: pos };
}

// The divisor the overlap-preset breakdown reads its seconds and shares by. Cluster total divides every
// quantity by the counted block count and keeps the pooled-time shares. Per node divides every quantity by
// the node data points carrying it and renormalizes the shares from those seconds.
export type PhaseTimingMode = 'clusterTotal' | 'perNode';

// Sum-based phase breakdown for an overlap preset, accumulated over node data points so unbalanced nodes
// weigh in by their real working time. A data point is one participating node of a counted successful
// block, and each contributes its per-phase window seconds, the measured overlap of every consecutive
// overlap-phase pair, and a Rest of its own working span that its phase windows leave uncovered. A node's
// span ends when its last phase does, so a node that finishes early carries no trailing idle while later
// nodes keep working. Two modes read the same accumulation. Cluster total divides every quantity by the
// counted block count, the mean summed node-time per block, and keeps the pooled-time shares so the layout
// matches the summed windows. Per node divides every quantity by the node data points carrying it, so
// concurrent phases read as per-node means while a single-node phase keeps its full duration, then
// renormalizes the shares from those seconds with each consecutive-pair band subtracted once so the row
// still closes at one. The layout tiles the phases across the unit row, consecutive overlap phases sharing
// a striped band the width of that pair's overlap share, with Rest filling the right end. It also reports
// each pair's mean overlap its striped band labels.
export function meanWindowPhases(
  blocks: Block[],
  registry: PhaseRegistry,
  mode: PhaseTimingMode = 'perNode'
): { phases: PhaseMean[]; total: number } {
  const phaseSum = registry.list.map(() => 0);
  // Node data points carrying each phase, the per-node divisor so a single-node phase keeps its full mean.
  const phaseCarry = registry.list.map(() => 0);
  // The overlap phases in order and the consecutive pairs between them, one striped band per pair.
  const overlapIndices = registry.list.flatMap((phase, i) => (phase.overlap ? [i] : []));
  const pairs = consecutivePairs(overlapIndices);
  const pairSum = pairs.map(() => 0);
  // Node data points where both phases of the pair are present, that band's per-node divisor, so a
  // node running both phases disjointly weighs its zero overlap in like phaseCarry weighs each phase.
  const pairCarry = pairs.map(() => 0);
  let restSum = 0;
  let totalSum = 0;
  let count = 0;
  // Counted successful blocks that contributed a data point, the cluster-total divisor.
  let blockCount = 0;
  for (const block of blocks) {
    if (block.status !== 'success') continue;
    let counted = false;
    for (const node of block.nodes) {
      if (!node.participated) continue;
      const windows = registry.list.map(phase => windowSeconds(node.phases[phase.index] ?? null));
      const present = windows.flatMap(w => (w ? [w] : []));
      if (!present.length) continue;
      windows.forEach((w, i) => {
        if (w) {
          phaseSum[i]! += w.end - w.start;
          phaseCarry[i]! += 1;
        }
      });
      // The node's working span runs from the block start to its last phase end, so trailing idle after
      // it finishes is not charged to it.
      const nodeEnd = Math.max(...present.map(w => w.end));
      pairs.forEach(([first, second], k) => {
        const overlap = pairOverlapSeconds(windows[first]!, windows[second]!);
        pairSum[k]! += overlap;
        if (windows[first] && windows[second]) pairCarry[k]! += 1;
      });
      restSum += Math.max(0, nodeEnd - unionLength(present));
      totalSum += nodeEnd;
      count += 1;
      counted = true;
    }
    if (counted) blockCount += 1;
  }

  // Per node divides each quantity by the data points carrying it, cluster total by the counted block
  // count. Rest is carried by every counted data point.
  const perNode = mode === 'perNode';
  const phaseSeconds = registry.list.map((_, i) =>
    perNode ? (phaseCarry[i]! ? phaseSum[i]! / phaseCarry[i]! : 0) : blockCount ? phaseSum[i]! / blockCount : 0
  );
  const restSeconds = perNode ? (count ? restSum / count : 0) : blockCount ? restSum / blockCount : 0;
  // Each pair's band seconds, per node over the nodes carrying it so a single-node band keeps its full
  // mean, cluster total over the counted blocks.
  const bandSeconds = pairs.map((_, k) =>
    perNode ? (pairCarry[k]! ? pairSum[k]! / pairCarry[k]! : 0) : blockCount ? pairSum[k]! / blockCount : 0
  );
  const total = perNode ? (count ? totalSum / count : 0) : blockCount ? totalSum / blockCount : 0;

  // Cluster total keeps the summed-window shares, a phase over the summed working spans. Per node
  // renormalizes the mode seconds by their sum less every consecutive-pair band, so the chain still tiles
  // the row and closes it at one with Rest at the right.
  const bandTotal = bandSeconds.reduce((sum, v) => sum + v, 0);
  const shareDenom = (perNode ? phaseSeconds.reduce((sum, v) => sum + v, 0) - bandTotal + restSeconds : totalSum) || 1;
  const pct = (perNode ? phaseSeconds : phaseSum).map(v => v / shareDenom);
  const bandPct = (perNode ? bandSeconds : pairSum).map(v => v / shareDenom);
  const restFrac = (perNode ? restSeconds : restSum) / shareDenom;
  const { placements, end } = placeSumPhases(registry.list, pct, overlapIndices, bandPct);
  const phases: PhaseMean[] = registry.list.map((phase, i) => ({
    key: phase.name,
    label: phase.label,
    seconds: phaseSeconds[i]!,
    placed: placements[i],
  }));
  // Rest fills the right end, its rendered width the remainder so the row closes at one while its label
  // and tooltip carry the mode's uncovered share and seconds.
  phases.push({
    key: 'rest',
    label: 'Rest',
    seconds: restSeconds,
    placed: { start: end, width: Math.max(0, 1 - end), frac: restFrac },
    hatched: true,
  });
  return { phases, total };
}

// Sub-phase milliseconds summed across a block population, indexed by the registry sub-phase order,
// which is the template order a block's rows reference. Only successful blocks count, matching the
// population meanWindowPhases subdivides, so a failed proof's sub-phase rows never skew the split.
export function subPhaseSums(blocks: Block[], registry: PhaseRegistry): number[] {
  const sums = registry.subphases.map(() => 0);
  for (const block of blocks) {
    if (block.status !== 'success') continue;
    for (const [index, ms] of block.subphases ?? []) {
      if (index < sums.length) sums[index]! += ms;
    }
  }
  return sums;
}

// Replaces each overlap phase that owns sub-phases with a run of tinted sub-bars subdividing its
// placement span proportionally to the population's sub-phase sums, so the segment and recursion bars
// split into their STARK sub-steps while the bar's envelope length is preserved and the row still
// closes at one. A population with no sub-phase data for a phase divides it equally so the bar stays
// filled and every row keeps identical sub-phase slots. Phases without sub-phases, and a preset with
// no template, pass through unchanged.
export function expandSubPhases(
  mean: { phases: PhaseMean[]; total: number },
  blocks: Block[],
  registry: PhaseRegistry
): { phases: PhaseMean[]; total: number } {
  if (!registry.subphases.length) return mean;
  const sums = subPhaseSums(blocks, registry);
  const phases = mean.phases.flatMap(phase => {
    const subs = registry.subphases.filter(sub => sub.phase === phase.key);
    if (!subs.length || !phase.placed) return [phase];
    const placed = phase.placed;
    const total = subs.reduce((acc, sub) => acc + sums[sub.index]!, 0);
    let cursor = placed.start;
    return subs.map(sub => {
      const share = total > 0 ? sums[sub.index]! / total : 1 / subs.length;
      const start = cursor;
      const width = placed.width * share;
      cursor += width;
      return {
        key: sub.key,
        label: sub.label,
        seconds: phase.seconds * share,
        placed: { start, width, frac: placed.frac * share },
      };
    });
  });
  return { phases, total: mean.total };
}
