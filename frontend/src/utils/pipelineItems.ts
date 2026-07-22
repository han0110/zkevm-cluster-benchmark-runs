/*
 * Decode layer for the fine-pipeline wire rows, the only module that knows the encoding. A node's rows
 * are variable-arity integer tuples [kind, s, d, s2, d2] in ms from the block start, decoded into
 * per-item segment windows in seconds, flattened across nodes, and start-sorted so the array index is
 * the timeline row. The row windowing math for the virtualized timeline lives here too, keeping the
 * chart pure rendering.
 */

import { msToSec } from '@/utils/format';
import type { PhaseRegistry } from '@/utils/phases';
import type { Benchmark, Block } from '@/types/benchmark';

// One drawn span of an item. A dangling span never closed, so its end is the clamp, not a recorded
// finish.
export interface PipelineSegment {
  startSec: number;
  endSec: number;
  dangling?: boolean;
}

// One decoded item, a row of the timeline. `segments` holds one span for a single-segment row and
// [witness, compute] for a full pair, with the envelope start and end spanning them.
export interface PipelineItem {
  kind: number;
  label: string;
  phase: string;
  paired: boolean;
  color: string;
  nodeIndex: number;
  nodeId: string;
  segments: PipelineSegment[];
  startSec: number;
  endSec: number;
}

// A block's decoded pipeline. Items sort by start with node then kind tie-breaks, phasesUsed lists the
// phases the items reach once each in template order, and endSec is the envelope end across every item.
export interface PipelineModel {
  items: PipelineItem[];
  phasesUsed: string[];
  endSec: number;
}

// Whether a block carries anything to decode, a non-empty kind template and rows on some node.
export function hasPipeline(bench: Benchmark, block: Block): boolean {
  return (
    (bench.software.zkvm.pipeline?.length ?? 0) > 0 && block.nodes.some(node => (node.pipeline?.length ?? 0) > 0)
  );
}

// Whether a wire row is well formed, arity 2 to 5 with every field finite. Applied in the clamp
// pre-scan and the item loop alike so one malformed row never skews the dangling clamp.
const validRow = (row: number[]): boolean =>
  row.length >= 2 && row.length <= 5 && row.every(v => Number.isFinite(v));

// Decodes a block's wire rows into the timeline model. A dangling segment's end clamps to its node's
// crash moment when marked, else to the block's latest known end, and a malformed row (bad arity,
// non-finite fields, kind outside the template) is dropped rather than thrown so one corrupt row never
// blanks the timeline.
export function decodePipeline(bench: Benchmark, block: Block, nodes: string[], registry: PhaseRegistry): PipelineModel {
  const kinds = bench.software.zkvm.pipeline ?? [];

  // The latest known moment of the block in ms, read from every node's phase ends, crash moment, and
  // completed pipeline segment ends.
  let knownEndMs = 0;
  for (const node of block.nodes) {
    for (const win of node.phases) if (win) knownEndMs = Math.max(knownEndMs, win.start_ms + win.dur_ms);
    if (node.crashed_ms != null) knownEndMs = Math.max(knownEndMs, node.crashed_ms);
    for (const row of node.pipeline ?? []) {
      if (!validRow(row)) continue;
      if (row.length >= 3) knownEndMs = Math.max(knownEndMs, row[1]! + row[2]!);
      if (row.length === 5) knownEndMs = Math.max(knownEndMs, row[3]! + row[4]!);
    }
  }

  const items: PipelineItem[] = [];
  block.nodes.forEach((node, nodeIndex) => {
    const clampMs = node.crashed_ms ?? knownEndMs;
    for (const row of node.pipeline ?? []) {
      if (!validRow(row)) continue;
      const kind = kinds[row[0]!];
      if (!kind) continue;
      const span = (startMs: number, durMs: number | undefined): PipelineSegment =>
        durMs != null
          ? { startSec: msToSec(startMs), endSec: msToSec(startMs + durMs) }
          : { startSec: msToSec(startMs), endSec: msToSec(Math.max(startMs, clampMs)), dangling: true };
      const segments = [span(row[1]!, row[2])];
      if (row.length >= 4) segments.push(span(row[3]!, row[4]));
      items.push({
        kind: row[0]!,
        label: kind.label,
        phase: kind.phase,
        paired: kind.paired,
        color: registry.color(kind.phase),
        nodeIndex,
        nodeId: nodes[nodeIndex] ?? `node${nodeIndex + 1}`,
        segments,
        startSec: segments[0]!.startSec,
        endSec: Math.max(...segments.map(s => s.endSec)),
      });
    }
  });
  items.sort((a, b) => a.startSec - b.startSec || a.nodeIndex - b.nodeIndex || a.kind - b.kind);

  const reached = new Set(items.map(item => item.phase));
  const phasesUsed = [...new Set(kinds.map(kind => kind.phase))].filter(phase => reached.has(phase));
  const endSec = items.reduce((max, item) => Math.max(max, item.endSec), 0);
  return { items, phasesUsed, endSec };
}

// Row pitch and bar height in px by item count, denser as the timeline grows so the scroll extent and
// the windowed canvas stay proportionate at any block size.
export function rowPitch(total: number): { pitch: number; bar: number } {
  if (total <= 800) return { pitch: 8, bar: 6 };
  if (total <= 3000) return { pitch: 5, bar: 4 };
  if (total <= 12000) return { pitch: 3, bar: 2 };
  return { pitch: 2, bar: 2 };
}

// Half-open row range covering the viewport plus overscan, the slice the timeline renders.
export function rowWindow(
  scrollTop: number,
  viewportPx: number,
  pitch: number,
  total: number,
  overscan: number
): { start: number; end: number } {
  const start = Math.max(0, Math.floor(scrollTop / pitch) - overscan);
  const end = Math.min(total, Math.ceil((scrollTop + viewportPx) / pitch) + overscan);
  return { start, end: Math.max(start, end) };
}
