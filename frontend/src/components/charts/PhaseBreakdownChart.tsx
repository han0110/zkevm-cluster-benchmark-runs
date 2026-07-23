/*
 * Mean phase breakdown across proving-time buckets, one share-mode StackedPhaseBars row per bucket so
 * reading top to bottom shows how the phase mix shifts as proofs get slower.
 */

import { StackedPhaseBars, type BarRow } from '@/components/charts/StackedPhaseBars';
import type { PhaseRegistry } from '@/utils/phases';
import { hasOverlapPhases, type PhaseMean } from '@/utils/phaseTimings';

export interface PhaseBreakdownRow {
  label: string;
  phases: PhaseMean[];
  total: number;
}

export function PhaseBreakdownChart({
  rows,
  registry,
  rowHeight = 26,
}: {
  rows: PhaseBreakdownRow[];
  registry: PhaseRegistry;
  rowHeight?: number;
}) {
  // Every phase mean carries a registry phase name, so its color comes straight from the registry. A
  // placement rides through so an overlap preset's row draws placed bars instead of a stack.
  const barRows: BarRow[] = rows.map(r => ({
    label: r.label,
    segments: r.phases.map(p => ({ key: p.key, label: p.label, color: registry.color(p.key), seconds: p.seconds, placed: p.placed, hatched: p.hatched })),
  }));
  // An overlap preset labels each rendered piece against the share-normalized row total of one, the piece
  // labeling the block trace uses, so every solid slice, striped band, and Rest reads its own share and
  // they sum to the whole. A presetless cluster keeps the classic per-phase stacked labels, marked by an
  // absent base.
  const pieceLabelBaseSec = hasOverlapPhases(registry) ? 1 : undefined;
  return <StackedPhaseBars rows={barRows} mode="share" rowHeight={rowHeight} pieceLabelBaseSec={pieceLabelBaseSec} />;
}
