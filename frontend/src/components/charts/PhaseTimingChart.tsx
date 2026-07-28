/*
 * Phase-timing line chart across blocks, one line per phase, legible at the 500-1000 block scale where
 * stacked bars crush into slivers. A preset whose phases overlap draws the total alone, since a line per
 * phase would read as a sequence the concurrent phases never form. With a controlled zoom window it
 * reports wheel and drag back so a sibling stays in step, otherwise it zooms self-contained.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { EChartsCoreOption } from 'echarts/core';
import { EChart, type ChartInstance } from '@/components/charts/EChart';
import { GroupedLegend, type LegendGroup } from '@/components/common/GroupedLegend';
import { useThemeColors } from '@/hooks/useThemeColors';
import { namedAxis, sliderDataZoom, parseDataZoom, grafanaSelect } from '@/utils/chartHelpers';
import { dash, formatSeconds } from '@/utils/format';
import type { PhaseRegistry } from '@/utils/phases';

// Block-time reference thresholds in seconds, drawn as dashed horizontal lines so a reader sees each
// block's proving time against the target.
const THRESHOLDS_S = [9, 12];

interface PhaseTimingChartProps {
  // X-axis category labels, one per block.
  labels: string[];
  // Per-block durations in seconds, keyed by phase name.
  values: Record<string, number[]>;
  registry: PhaseRegistry;
  // Optional per-block total proof time, drawn as the leading solid envelope line.
  total?: number[];
  // Draws the total alone, dropping the per-phase lines and the legend that selects them. Set for a
  // preset whose phases overlap, where a per-phase line misreports concurrency as sequence.
  totalOnly?: boolean;
  height?: number;
  // Zoom window in percent, read on rebuild so it survives a legend toggle without being an option dep
  // (which would rebuild per wheel tick). Live wheel/drag report through onZoom and apply to the
  // instance, so the chart is never controlled.
  getZoom?: () => [number, number];
  onZoom?: (start: number, end: number) => void;
  // Reports the hovered block index so a synced view can mark the matching window, null on mouse out.
  onHoverBlock?: (index: number | null) => void;
  // Smallest zoom window in categories so a deep zoom holds at a legible block count.
  minValueSpan?: number;
  onReady?: (instance: ChartInstance) => void;
}

export function PhaseTimingChart({
  labels,
  values,
  registry,
  total,
  totalOnly = false,
  height = 320,
  getZoom,
  onZoom,
  onHoverBlock,
  minValueSpan = 2,
  onReady,
}: PhaseTimingChartProps) {
  const theme = useThemeColors();

  // The phases drawn as their own line, none of them in total-only mode. Every per-phase concern below
  // reads this rather than the registry, so one branch decides the whole shape of the chart.
  const phases = useMemo(() => (totalOnly ? [] : registry.list), [totalOnly, registry]);

  // Legend identities keyed by series name so the selection set drives visibility. Total leads with a
  // solid swatch matching its envelope line, then one dot per drawn phase.
  const items = useMemo(() => {
    const phaseItems = phases.map(p => ({ key: p.label, label: p.label, color: p.color }));
    return total ? [{ key: 'Total', label: 'Total', color: theme.muted }, ...phaseItems] : phaseItems;
  }, [phases, total, theme]);

  const allKeys = useMemo(() => items.map(i => i.key), [items]);
  // Grafana-style isolation. Opens with every series enabled and resets to all when the series set
  // changes (switching run or focused node).
  const sig = allKeys.join(',');
  const [selected, setSelected] = useState<Set<string>>(() => new Set(allKeys));
  useEffect(() => setSelected(new Set(sig.split(','))), [sig]);

  // Y-axis ceiling fixed over every series including the total envelope and the reference thresholds, so
  // isolating one phase never rescales the axis and both threshold lines stay in view.
  const yMax = useMemo(() => {
    const all = [...(total ?? []), ...phases.flatMap(p => values[p.name] ?? []), ...THRESHOLDS_S];
    const peak = all.reduce((m, v) => (v != null && v > m ? v : m), 0);
    return peak > 0 ? Math.ceil(peak * 1.05) : 1;
  }, [values, phases, total]);

  const option = useMemo<EChartsCoreOption>(() => {
    // Symbols off so a thousand-block run reads as clean trends, lttb sampling keeps the path light while
    // preserving peaks.
    const line = (name: string, color: string, data: number[] | undefined, dashed = false) => ({
      name,
      type: 'line' as const,
      showSymbol: false,
      sampling: 'lttb' as const,
      lineStyle: { color, width: dashed ? 1.4 : 1.6, ...(dashed ? { type: 'dashed' as const } : {}) },
      itemStyle: { color },
      emphasis: { focus: 'series' as const },
      data: data ?? [],
    });
    // Total leads as a solid envelope, then one line per drawn phase. Only selected series are
    // emitted so isolating a phase hides the rest without disturbing the fixed axes.
    const series = [
      ...(total ? [line('Total', theme.muted, total)] : []),
      ...phases.map(phase => line(phase.label, phase.color, values[phase.name])),
    ].filter(s => selected.has(s.name));

    // Dashed reference thresholds on a silent series so they always show regardless of the selection
    // and never join the tooltip, each labelled with its second value at the right edge.
    const thresholds = {
      type: 'line' as const,
      data: [] as number[],
      silent: true,
      markLine: {
        silent: true,
        symbol: 'none' as const,
        animation: false,
        lineStyle: { color: theme.faint, type: 'dashed' as const, width: 1 },
        label: { show: true, position: 'insideEndTop' as const, formatter: '{c}s', color: theme.faint, fontSize: 10 },
        data: THRESHOLDS_S.map(y => ({ yAxis: y })),
      },
    };

    return {
      // Animation off so a wheel zoom applies instantly, keeping scroll smooth and matching the synced
      // telemetry panels.
      animation: false,
      grid: { left: 48, right: 18, top: 12, bottom: 64, containLabel: true },
      // Selectable legend is the elevated GroupedLegend above the chart, so the native one is off.
      legend: { show: false },
      // Tick labels hidden because a run's proofs may be arbitrary fixtures not consecutive blocks, so a
      // dense row of long ids would not read. The axis keeps its categories so zoom and sync work by
      // index.
      xAxis: { type: 'category', data: labels, ...namedAxis('Block', 12), axisLabel: { show: false } },
      // Ceiling fixed over all series so the axis holds steady as the selection changes.
      yAxis: { type: 'value', min: 0, max: yMax, ...namedAxis('Seconds', 36) },
      tooltip: {
        trigger: 'axis',
        valueFormatter: (v: number | null) => dash(v, formatSeconds),
      },
      dataZoom: sliderDataZoom(theme, ...(getZoom ? getZoom() : [0, 100]), minValueSpan),
      series: [...series, thresholds],
    };
    // getZoom is read for the window but kept out of the deps on purpose so a wheel does not rebuild the
    // option, the window is only re-read when the series or axis change.
  }, [labels, values, phases, total, theme, selected, yMax, getZoom, minValueSpan]);

  // Smallest zoom window as a percentage so the wheel guard knows when the chart is at its floor and
  // cannot zoom in further.
  const floorPct = labels.length > 1 ? (minValueSpan / (labels.length - 1)) * 100 : 100;

  // Wheel and hover bind through the live instance, not the wrapper's onEvents which can miss a rebind.
  // Refs hold the latest callbacks so once-bound handlers never go stale, and the leading off clears any
  // handler left on a reconnected instance.
  const onZoomRef = useRef(onZoom);
  const onHoverRef = useRef(onHoverBlock);
  const floorRef = useRef(floorPct);
  // Current window tracked from the chart's own datazoom events so the wheel guard reads it without a
  // costly getOption. Re-synced on render to the persisted window when synced, else to full, because a
  // standalone chart resets to full on rebuild.
  const zoomRef = useRef<[number, number]>([0, 100]);
  useEffect(() => {
    onZoomRef.current = onZoom;
    onHoverRef.current = onHoverBlock;
    floorRef.current = floorPct;
    zoomRef.current = getZoom ? getZoom() : [0, 100];
  });
  // Captured before echarts so a wheel that would zoom past the floor is swallowed entirely, leaving the
  // window put rather than letting echarts recentre it and drift. Zooming out is left to echarts.
  // Applies to every phase chart, synced or standalone, so the two behave identically.
  const onWheelRef = useRef((e: WheelEvent) => {
    const [s, en] = zoomRef.current;
    if (en - s <= floorRef.current + 0.5 && e.deltaY < 0) {
      e.preventDefault();
      e.stopPropagation();
    }
  });
  const handleReady = useCallback(
    (inst: ChartInstance) => {
      inst.off('datazoom');
      inst.off('updateAxisPointer');
      inst.off('globalout');
      inst.on('datazoom', (p: unknown) => {
        const z = parseDataZoom(p);
        if (z) {
          zoomRef.current = [z.start, z.end];
          onZoomRef.current?.(z.start, z.end);
        }
      });
      inst.on('updateAxisPointer', (p: unknown) => {
        const v = (p as { axesInfo?: Array<{ value?: number | string }> }).axesInfo?.[0]?.value;
        onHoverRef.current?.(v == null ? null : Number(v));
      });
      inst.on('globalout', () => onHoverRef.current?.(null));
      const dom = inst.getDom();
      dom.removeEventListener('wheel', onWheelRef.current, true);
      dom.addEventListener('wheel', onWheelRef.current, { capture: true, passive: false });
      onReady?.(inst);
    },
    [onReady]
  );

  // One legend group of phase identities. Plain click isolates a series, click on the lone selected one
  // resets to all, Cmd/Ctrl click toggles one, matching the GPU telemetry legend.
  const legendGroups: LegendGroup[] = useMemo(
    () => [{ items, selected, onToggle: (key, multi) => setSelected(prev => grafanaSelect(prev, allKeys, key, multi)) }],
    [items, selected, allKeys]
  );

  // A lone series has nothing to select between, so the legend appears only once there are at least two.
  return (
    <div className="flex flex-col gap-2">
      {items.length > 1 && <GroupedLegend groups={legendGroups} orientation="horizontal" />}
      <EChart option={option} height={height} onReady={handleReady} />
    </div>
  );
}
