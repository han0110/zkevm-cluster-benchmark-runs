/*
 * Per-node proving timeline as a time-mode StackedPhaseBars, one row per node. Every row emits the same
 * fixed gap-and-fill column layout in registry order so the stacked-bar component reads slot colors and
 * labels from the first row and still aligns every node. An unrun phase emits a zero-width fill.
 */

import { useMemo } from 'react';
import { StackedPhaseBars, type BarMarker, type BarRow, type BarSegment } from '@/components/charts/StackedPhaseBars';
import { traceLabelBaseSec, windowSeconds } from '@/utils/phaseTimings';
import { resolveCssColorToHex } from '@/utils/color';
import { msToSec } from '@/utils/format';
import type { PhaseRegistry } from '@/utils/phases';
import type { Block } from '@/types/benchmark';

// Marker colors, maple red for a crash and faint grey for a cancel. Resolved lazily on first use, not at
// module load, because resolving before index.css is applied (this module is imported before the
// stylesheet) computes an unresolved var() to inherited near-white. By first render the stylesheet is in
// place so the read returns the real color.
let markerColors: { crash: string; cancel: string } | null = null;
function getMarkerColors(): { crash: string; cancel: string } {
  if (!markerColors) {
    markerColors = {
      crash: resolveCssColorToHex('var(--color-crash)'),
      cancel: resolveCssColorToHex('var(--color-cancel)'),
    };
  }
  return markerColors;
}

export function Gantt({
  block,
  nodes,
  registry,
  height = 250,
  cursorSec,
}: {
  block: Block;
  nodes: string[];
  registry: PhaseRegistry;
  height?: number;
  // A full-height dashed cursor line at this second, set by the fullscreen log console to point at the
  // trace moment a hovered log line was emitted.
  cursorSec?: number | null;
}) {
  // Memoized on the inputs the rows derive from so a cursor-only re-render reuses the prior arrays and
  // does not invalidate the chart option memo downstream.
  const rows = useMemo<BarRow[]>(() => {
    const seg = (key: string, label: string, color: string, seconds: number, transparent = false): BarSegment => ({
      key,
      label,
      color,
      seconds: Math.max(0, seconds),
      transparent,
    });

    // Overlap phases draw at their absolute windows so their concurrent spans share the row, the
    // renderer striping any interval two of them cover at once.
    const overlapPhases = new Set(registry.list.filter(p => p.overlap).map(p => p.name));

    return block.nodes.map((node, nodeIndex) => {
      // Each phase window in seconds since block start, from the node's own windows. The aggregate
      // (last) window is null on every node but the aggregator.
      const windows = registry.list.map(phase => windowSeconds(node.phases[phase.index] ?? null));

      const segments: BarSegment[] = [];
      let prevEnd = 0;
      registry.list.forEach((phase, i) => {
        const win = windows[i];
        if (overlapPhases.has(phase.name)) {
          // An overlap phase draws at its absolute window, leaving the stack cursor put so a
          // concurrent sibling can occupy the same span.
          const start = win ? Math.max(win.start, 0) : 0;
          const end = win ? Math.max(start, win.end) : start;
          segments.push(seg(`gap-${phase.name}`, 'gap', 'transparent', 0, true));
          segments.push({
            ...seg(phase.name, phase.label, phase.color, end - start),
            placed: { start, width: end - start },
          });
          return;
        }
        // Every phase emits a gap-and-fill pair so columns align across rows, an absent phase
        // contributing zero-width segments that keep their color. Each window is clamped to the axis
        // origin and to any earlier phase so an overlapping or pre-start window cannot inflate the bar
        // past the node's real end.
        const start = win ? Math.max(prevEnd, win.start, 0) : prevEnd;
        const end = win ? Math.max(start, win.end) : prevEnd;
        segments.push(seg(`gap-${phase.name}`, 'gap', 'transparent', start - prevEnd, true));
        segments.push(seg(phase.name, phase.label, phase.color, end - start));
        prevEnd = end;
      });
      // A node that took no part is drawn as a hatched ghost band not empty phases, so it reads as
      // absent from the run rather than a node that did nothing.
      const label = nodes[nodeIndex] ?? `node${nodeIndex + 1}`;
      return node.participated
        ? { label, segments }
        : { label, segments, absent: true, absentLabel: 'did not participate' };
    });
  }, [block, nodes, registry]);

  // Each node that ended early gets a marker at its end second, a crash for the lost node and a cancel
  // for one stopped after a sibling crashed. Memoized on the block so a cursor-only re-render reuses the
  // prior array.
  const markers = useMemo<BarMarker[]>(() => {
    const colors = getMarkerColors();
    return block.nodes.flatMap((node, nodeIndex) => {
      if (node.crashed_ms == null) return [];
      const cancelled = node.crash_kind === 'cancelled';
      return [
        {
          row: nodeIndex,
          seconds: msToSec(node.crashed_ms),
          label: cancelled ? 'cancel' : 'crash',
          color: cancelled ? colors.cancel : colors.crash,
        },
      ];
    });
  }, [block]);

  return (
    <StackedPhaseBars
      rows={rows}
      mode="time"
      height={height}
      markers={markers}
      cursorSec={cursorSec}
      pieceLabelBaseSec={traceLabelBaseSec(block, registry)}
    />
  );
}
