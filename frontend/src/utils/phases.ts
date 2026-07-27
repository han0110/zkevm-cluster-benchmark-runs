/*
 * Per-run phase registry, the single source of phase identity, order, label, and color, built from
 * software.zkvm.phases (phases are data not code). Colors come from each zkVM's preset by phase name,
 * falling back to the palette by position so a presetless cluster still renders distinct fills. The
 * aggregator-only phase is derived generically, not by hardcoded name.
 */

import type { Benchmark, Block } from '@/types/benchmark';
import { resolveCssColorToHex, tints } from '@/utils/color';

// Per-zkVM phase color presets, keyed by zkVM name then phase name, as index.css variables. zisk routes
// its input lead-in to the muted fill, and prove avoids the maple-red reserved for the crash marker so
// the prove fill and crash line never share a color.
const PHASE_PRESETS: Record<string, Record<string, string>> = {
  zisk: {
    input: 'var(--color-phase-muted)',
    emulation: 'var(--color-phase-1)',
    commit: 'var(--color-phase-2)',
    prove: 'var(--color-phase-3)',
    aggregate: 'var(--color-phase-4)',
  },
  openvm: {
    input: 'var(--color-phase-muted)',
    execution: 'var(--color-phase-1)',
    segment: 'var(--color-phase-2)',
    recursion: 'var(--color-phase-3)',
    wrap: 'var(--color-phase-4)',
  },
};

// Position-indexed fallback palette for phases (or whole zkVMs) absent from a preset. At least five
// distinct fills so an unrecognized cluster renders legibly, wrapping for longer presets.
const FALLBACK_PALETTE = [
  'var(--color-phase-1)',
  'var(--color-phase-2)',
  'var(--color-phase-3)',
  'var(--color-phase-4)',
  'var(--color-phase-5)',
  'var(--color-phase-6)',
];

// Resolves a phase color from its zkVM preset by name, falling back to the palette by position.
const phaseColor = (zkvm: string, name: string, index: number): string => {
  const preset = PHASE_PRESETS[zkvm]?.[name];
  const fallback = FALLBACK_PALETTE[index % FALLBACK_PALETTE.length];
  return resolveCssColorToHex(preset ?? fallback ?? '#888888');
};

export interface PhaseEntry {
  name: string;
  label: string;
  index: number;
  color: string;
  // True for a phase whose windows run concurrently with an adjacent overlap phase.
  overlap: boolean;
}

// One sub-phase of the fine STARK breakdown, colored as a tint of its owning coarse phase.
export interface SubPhaseEntry {
  name: string;
  label: string;
  // Owning coarse phase name, the tint source.
  phase: string;
  // Position in the wire template.
  index: number;
  // Composite `${phase}::${name}`, the registry color lookup key unique across the two owners.
  key: string;
  color: string;
}

// The registry color lookup key for a sub-phase, unique across the segment and recursion owners.
export const subPhaseKey = (phase: string, name: string): string => `${phase}::${name}`;

export interface PhaseRegistry {
  list: PhaseEntry[];
  // The sub-phase breakdown template with tint colors, empty when the run's logs carried no per-item spans.
  subphases: SubPhaseEntry[];
  byName(name: string): PhaseEntry | undefined;
  color(name: string): string;
  label(name: string): string;
  // Name of the single phase that runs on the aggregator node only, or null when none.
  aggregatorPhase: string | null;
}

const cache = new WeakMap<Benchmark, PhaseRegistry>();

// Builds (and memoizes by benchmark identity) the phase registry for a loaded run.
export function buildPhaseRegistry(benchmark: Benchmark): PhaseRegistry {
  const existing = cache.get(benchmark);
  if (existing) return existing;

  const zkvm = benchmark.software.zkvm.name;
  const list: PhaseEntry[] = benchmark.software.zkvm.phases.map((phase, index) => ({
    name: phase.name,
    label: phase.label,
    index,
    color: phaseColor(zkvm, phase.name, index),
    overlap: phase.overlap === true,
  }));
  const byName = new Map(list.map(entry => [entry.name, entry]));

  const subphases = buildSubPhases(benchmark, byName);
  const colorByKey = new Map<string, string>(list.map(entry => [entry.name, entry.color]));
  for (const sub of subphases) colorByKey.set(sub.key, sub.color);

  const aggregatorPhase = deriveAggregatorPhase(benchmark, list);

  const registry: PhaseRegistry = {
    list,
    subphases,
    byName: name => byName.get(name),
    color: name => colorByKey.get(name) ?? '#888888',
    label: name => byName.get(name)?.label ?? name,
    aggregatorPhase,
  };
  cache.set(benchmark, registry);
  return registry;
}

// The sub-phase template with tint colors, each coarse phase's sub-phases sharing a lightness ramp of
// the parent color so the segment and recursion breakdowns read as shades of their bar. Empty when
// the document carries no sub-phase template.
function buildSubPhases(benchmark: Benchmark, byName: Map<string, PhaseEntry>): SubPhaseEntry[] {
  const defs = benchmark.software.zkvm.subphases ?? [];
  const counts = new Map<string, number>();
  for (const def of defs) counts.set(def.phase, (counts.get(def.phase) ?? 0) + 1);
  const ramps = new Map<string, string[]>();
  for (const [phase, count] of counts) ramps.set(phase, tints(byName.get(phase)?.color ?? '#888888', count));
  const seen = new Map<string, number>();
  return defs.map((def, index) => {
    const position = seen.get(def.phase) ?? 0;
    seen.set(def.phase, position + 1);
    return {
      name: def.name,
      label: def.label,
      phase: def.phase,
      index,
      key: subPhaseKey(def.phase, def.name),
      color: ramps.get(def.phase)?.[position] ?? '#888888',
    };
  });
}

// Finds the last phase present on some but not all nodes of a block, the aggregator-only phase. The
// search runs last to first because an earlier phase can also sit on a subset of nodes, as recursion
// does when every segment of a node aggregates elsewhere. The search spans every run because the
// cluster shape is shared and a run may hold no clean block to read it from.
function deriveAggregatorPhase(benchmark: Benchmark, list: PhaseEntry[]): string | null {
  const block = benchmark.runs
    .flatMap(r => r.blocks)
    .find(b => b.status === 'success' && b.nodes.length > 0);
  if (!block) return null;
  for (const entry of [...list].reverse()) {
    const present = block.nodes.filter(n => n.phases[entry.index] != null).length;
    if (present > 0 && present < block.nodes.length) return entry.name;
  }
  return null;
}

// Display label for a block, its metric file name, which doubles as its stable identifier.
export const blockLabel = (block: Block): string => block.name;
