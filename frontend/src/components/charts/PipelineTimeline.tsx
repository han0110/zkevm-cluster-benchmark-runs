/*
 * Virtualized waterfall timeline of a block's decoded pipeline items, one thin row per item in start
 * order, drawn as a painted band inside the parent's scroll container. echarts 6.1.0 leaks whatever a
 * custom series paints whenever the series re-lays-out on a live instance, leaving stale bars as a
 * permanent canvas underlayer, so the band never updates in place. Its axes are fixed for the life of
 * an instance, the spacer div holds the full row extent, and the canvas sits absolutely at the
 * window's row offset with one row per pitch, so native scrolling simply reveals it. Scrolling past
 * the overscan hysteresis or changing the zoom remounts the chart at the new window. Each row draws a
 * node-colored gutter cell, its marked CPU heavy segment translucent under its solid siblings, and an
 * invisible full-pitch hit rect so hair-thin bars stay hoverable. Hovering draws a dashed crosshair
 * line following the cursor across the plot and reports the hovered moment through onHover for the
 * caller's readout chip. A raw zrender line drives the crosshair since the built-in axis pointer does
 * not repaint in place on this static band any more than the painted bars do. The time axis and zoom
 * slider live in the sibling PipelineTimeStrip,
 * a live instance of built-in components only, whose slider drives the shared zoom window feeding the
 * band's x extent.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type RefObject } from 'react';
import { graphic } from 'echarts/core';
import type { EChartsCoreOption } from 'echarts/core';
import type {
  CustomSeriesRenderItemAPI,
  CustomSeriesRenderItemParams,
  CustomSeriesRenderItemReturn,
} from 'echarts';
import { EChart, type ChartInstance } from '@/components/charts/EChart';
import { packRows, rowPitch, rowWindow, type PipelineItem, type PipelineSegment } from '@/utils/pipelineItems';
import { hexA, parseDataZoom, sliderOnlyDataZoom } from '@/utils/chartHelpers';
import { useThemeColors } from '@/hooks/useThemeColors';
import { nodeColorById } from '@/utils/dataVizColors';
import { formatSeconds } from '@/utils/format';
import type { PhaseRegistry } from '@/utils/phases';

// Rows kept rendered beyond the viewport so a scroll within the hysteresis band never shows a gap.
const OVERSCAN = 150;

// Width in px of the node color gutter hugging the plot's left edge.
const GUTTER = 8;

// Horizontal grid insets shared by the band and the time strip so their ticks align.
const PLOT_LEFT = GUTTER + 6;
const PLOT_RIGHT = 16;

// Tooltip color marker in the ECharts style, a small round dot ahead of its label.
const marker = (color: string): string =>
  `<span style="display:inline-block;width:8px;height:8px;border-radius:50%;background:${color};margin-right:6px"></span>`;

// One aligned tooltip row, the name then the right-aligned window and duration cells.
const tooltipRow = (name: string, window: string, duration: string): string =>
  `<tr><td>${name}</td><td style="text-align:right;padding-left:12px;white-space:nowrap">${window}</td><td style="text-align:right;padding-left:10px;white-space:nowrap">${duration}</td></tr>`;

interface Plot {
  x: number;
  y: number;
  width: number;
  height: number;
}

// Monotonic source for the per-items-identity remount key, shared across instances since only
// change detection matters.
let itemsRevisionCounter = 0;

export function PipelineTimeline({
  items,
  endSec,
  zoom,
  onHover,
  registry,
  scrollRef,
}: {
  // Decoded items in row order, already node-filtered by the caller.
  items: PipelineItem[];
  // Time-axis extent in seconds, the unfiltered block envelope so the axis holds across filters.
  endSec: number;
  // The strip's zoom window in percent of the extent, mapped onto the band's x-axis min and max.
  zoom: [number, number];
  // The hovered moment and its canvas x, null when the cursor is outside the plot or the canvas.
  onHover: (hover: { sec: number; x: number } | null) => void;
  registry: PhaseRegistry;
  // The parent's overflow-y-auto container, listened to for scroll and measured for the viewport.
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const theme = useThemeColors();
  const rows = useMemo(() => packRows(items), [items]);
  const total = rows.length;
  const { pitch, bar } = rowPitch(total);

  // Flat draw list keyed by row, one entry per bar-carrying item so each bar hovers to its own
  // tooltip. `rowStart[r]` is the flat index where row r begins, bounding the window slice.
  const { drawItems, drawRows, rowStart } = useMemo(() => {
    const drawItems: PipelineItem[] = [];
    const drawRows: number[] = [];
    const rowStart: number[] = new Array(rows.length + 1);
    rows.forEach((row, r) => {
      rowStart[r] = drawItems.length;
      for (const item of row.items) {
        drawItems.push(item);
        drawRows.push(r);
      }
    });
    rowStart[rows.length] = drawItems.length;
    return { drawItems, drawRows, rowStart };
  }, [rows]);

  // The items identity is the intended trigger, bumping the revision the remount key reads.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const itemsRevision = useMemo(() => ++itemsRevisionCounter, [items]);

  const [viewportPx, setViewportPx] = useState(0);
  const [anchorY, setAnchorY] = useState(0);
  const anchorRef = useRef(0);
  const frameRef = useRef(0);

  // Scroll and resize feed one rAF-coalesced reader maintaining the viewport size and the render
  // anchor, which moves only once the offset drifts past half the overscan, so most scrolling just
  // reveals the painted band and only an anchor jump remounts it.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const apply = (): void => {
      frameRef.current = 0;
      setViewportPx(el.clientHeight);
      const top = el.scrollTop;
      if (Math.abs(top - anchorRef.current) > (OVERSCAN / 2) * pitch) {
        anchorRef.current = top;
        setAnchorY(top);
      }
    };
    const onScroll = (): void => {
      if (!frameRef.current) frameRef.current = requestAnimationFrame(apply);
    };
    const observer = new ResizeObserver(onScroll);
    el.addEventListener('scroll', onScroll, { passive: true });
    observer.observe(el);
    apply();
    return () => {
      el.removeEventListener('scroll', onScroll);
      observer.disconnect();
      if (frameRef.current) cancelAnimationFrame(frameRef.current);
      frameRef.current = 0;
    };
  }, [scrollRef, pitch]);

  const win = useMemo(
    () => rowWindow(anchorY, viewportPx, pitch, total, OVERSCAN),
    [anchorY, viewportPx, pitch, total]
  );

  // Both axes are hidden and fixed, y to the window's rows so one row unit is exactly one pitch of
  // the spacer, x to the zoom window in seconds.
  const option = useMemo<EChartsCoreOption>(() => {
    const renderItem = (
      params: CustomSeriesRenderItemParams,
      api: CustomSeriesRenderItemAPI
    ): CustomSeriesRenderItemReturn => {
      const row = api.value(0) as number;
      const item = drawItems[api.value(1) as number]!;
      const plot = params.coordSys as unknown as Plot;
      const top = (api.coord([0, row]) as [number, number])[1];
      const pitchPx = (api.size!([0, 1]) as number[])[1]!;
      const clampX = (v: number): number => Math.min(Math.max(v, plot.x), plot.x + plot.width);
      const children: object[] = [
        // The node gutter cell hugging the plot's left edge, the row's node read as color.
        {
          type: 'rect',
          shape: { x: plot.x - GUTTER, y: top, width: GUTTER, height: pitchPx },
          style: { fill: nodeColorById(item.nodeId) },
        },
      ];
      const barY = top + (pitchPx - bar) / 2;
      // Segment spans in px at a 1px minimum width, clamped into the plot so a zoomed-out or dangling
      // end never paints past the band edges.
      const spans = item.segments.map(seg => {
        const x0 = (api.coord([seg.startSec, row]) as [number, number])[0];
        const x1 = (api.coord([seg.endSec, row]) as [number, number])[0];
        return [x0, Math.max(x1, x0 + 1)] as const;
      });
      spans.forEach(([x0, x1], i) => {
        const cx0 = clampX(x0);
        const cx1 = clampX(x1);
        if (cx1 <= cx0) return;
        // The marked CPU heavy segment draws translucent so an overlapping GPU heavy sibling sits
        // solid on top, while an unmarked item paints every segment at full opacity.
        const fill = i === item.cpuHeavy ? hexA(item.color, 0.45) : item.color;
        children.push({
          type: 'rect',
          shape: { x: cx0, y: barY, width: cx1 - cx0, height: bar },
          style: { fill },
        });
      });
      // An invisible full-pitch hit rect across the row envelope keeps hair-thin bars hoverable.
      const hitX0 = clampX(Math.min(...spans.map(s => s[0])));
      const hitX1 = clampX(Math.max(...spans.map(s => s[1])));
      if (hitX1 > hitX0) {
        children.push({
          type: 'rect',
          shape: { x: hitX0, y: top, width: hitX1 - hitX0, height: pitchPx },
          style: { fill: 'rgba(0,0,0,0)' },
        });
      }
      return { type: 'group', children } as CustomSeriesRenderItemReturn;
    };

    return {
      animation: false,
      grid: { left: PLOT_LEFT, right: PLOT_RIGHT, top: 0, bottom: 0, containLabel: false },
      xAxis: {
        type: 'value' as const,
        show: false,
        min: (endSec * zoom[0]) / 100,
        max: (endSec * zoom[1]) / 100,
      },
      yAxis: { type: 'value' as const, inverse: true, show: false, min: win.start, max: win.end },
      tooltip: {
        trigger: 'item' as const,
        confine: true,
        formatter: (p: unknown) => {
          const item = drawItems[(p as { value: [number, number] }).value[1]];
          if (!item) return '';
          // A dangling segment never finished, so its row shows an open range from the start and
          // (unfinished) in place of a duration. A lone unmarked segment collapses into the total
          // row alone, while a multi-segment or marked item lists its segments above the total,
          // each named by its CPU or GPU heavy marker and left unnamed without one.
          const segmentWindow = (seg: PipelineSegment): string =>
            seg.dangling
              ? `${formatSeconds(seg.startSec)} -`
              : `${formatSeconds(seg.startSec)} - ${formatSeconds(seg.endSec)}`;
          const segmentDuration = (seg: PipelineSegment): string =>
            seg.dangling ? '(unfinished)' : `(${formatSeconds(seg.endSec - seg.startSec)})`;
          const sideName = (i: number): string =>
            i === item.cpuHeavy ? 'CPU heavy' : i === item.gpuHeavy ? 'GPU heavy' : '';
          const rows =
            item.segments.length === 1 && sideName(0) === ''
              ? tooltipRow('total', segmentWindow(item.segments[0]!), segmentDuration(item.segments[0]!))
              : item.segments
                  .map((seg, i) => tooltipRow(sideName(i), segmentWindow(seg), segmentDuration(seg)))
                  .join('') + tooltipRow('total', '', formatSeconds(item.endSec - item.startSec));
          return (
            `<b>${item.label}${item.id != null ? ` #${item.id}` : ''}</b><br/>` +
            `${marker(nodeColorById(item.nodeId))}${item.nodeId}<br/>` +
            `${marker(item.color)}${registry.label(item.phase)}` +
            `<table style="border-collapse:collapse;margin-top:3px">${rows}</table>`
          );
        },
      },
      series: [
        {
          type: 'custom' as const,
          z: 3,
          data: Array.from({ length: rowStart[win.end]! - rowStart[win.start]! }, (_, i) => {
            const flat = rowStart[win.start]! + i;
            return [drawRows[flat]!, flat];
          }),
          renderItem,
        },
      ],
    };
  }, [drawItems, drawRows, rowStart, win, zoom, endSec, bar, registry]);

  // A zrender mousemove drives the dashed crosshair and reports the hovered time and canvas x for the
  // caller's readout, hitting the whole canvas rather than only series shapes, hiding the crosshair and
  // reporting null on leaving the plot or the canvas. The crosshair line sits on its own zlevel so
  // moving it repaints only its layer and never the painted band. Attached once per live instance,
  // which onReady hands over again after the dev StrictMode recreate.
  const onHoverRef = useRef(onHover);
  useEffect(() => {
    onHoverRef.current = onHover;
  }, [onHover]);
  const themeRef = useRef(theme);
  useEffect(() => {
    themeRef.current = theme;
  }, [theme]);
  const crosshairRef = useRef<graphic.Line | null>(null);
  useEffect(() => {
    crosshairRef.current?.setStyle({ stroke: theme.faint });
  }, [theme]);
  const instanceRef = useRef<ChartInstance | null>(null);
  const handleReady = useCallback((instance: ChartInstance): void => {
    if (instanceRef.current === instance) return;
    instanceRef.current = instance;
    const zr = instance.getZr();
    const crosshair = new graphic.Line({
      silent: true,
      ignore: true,
      zlevel: 1,
      style: { stroke: themeRef.current.faint, lineWidth: 1, lineDash: [4, 4] },
    });
    crosshairRef.current = crosshair;
    zr.add(crosshair);
    zr.on('mousemove', (e: { offsetX: number }) => {
      const inPlot = e.offsetX >= PLOT_LEFT && e.offsetX <= instance.getWidth() - PLOT_RIGHT;
      const sec = instance.convertFromPixel({ xAxisIndex: 0 }, e.offsetX);
      const hover = inPlot && Number.isFinite(sec) ? { sec, x: e.offsetX } : null;
      if (hover) {
        crosshair.setShape({ x1: e.offsetX, y1: 0, x2: e.offsetX, y2: instance.getHeight() });
        crosshair.show();
      } else {
        crosshair.hide();
      }
      onHoverRef.current(hover);
    });
    zr.on('globalout', () => {
      crosshair.hide();
      onHoverRef.current(null);
    });
  }, []);

  // The spacer holds the full row extent for the native scrollbar while the band canvas sits at the
  // window's row offset, so scrolling past the band edge shows blank spacer until the anchor jump
  // remounts the band at the new offset.
  const bandPx = (win.end - win.start) * pitch;
  return (
    <div className="relative" style={{ height: total * pitch }}>
      {bandPx > 0 && (
        <div className="absolute inset-x-0" style={{ top: win.start * pitch }}>
          <EChart
            key={`${itemsRevision}:${win.start}:${win.end}:${zoom[0]}:${zoom[1]}`}
            option={option}
            height={bandPx}
            onReady={handleReady}
          />
        </div>
      )}
    </div>
  );
}

// Slim live-instance strip holding the band's time axis and zoom slider, built-in components only so
// it updates cleanly in place. The axis spans the full extent windowed by the zoom, and the slider
// drives the shared zoom state through its datazoom events.
export function PipelineTimeStrip({
  endSec,
  zoom,
  onZoom,
}: {
  endSec: number;
  zoom: [number, number];
  onZoom: (zoom: [number, number]) => void;
}) {
  const theme = useThemeColors();
  const onEvents = useMemo(
    () => ({
      datazoom: (p: unknown): void => {
        const z = parseDataZoom(p);
        if (z) onZoom([z.start, z.end]);
      },
    }),
    [onZoom]
  );
  // The grid top inset holds the axis labels row and the bottom inset the slider, leaving a sliver
  // plot between them.
  const option = useMemo<EChartsCoreOption>(
    () => ({
      animation: false,
      grid: { left: PLOT_LEFT, right: PLOT_RIGHT, top: 24, bottom: 22, containLabel: false },
      xAxis: {
        type: 'value' as const,
        position: 'top' as const,
        min: 0,
        max: endSec,
        axisLabel: { formatter: (v: number) => `${+v.toFixed(3)} s` },
      },
      yAxis: { type: 'value' as const, show: false },
      dataZoom: sliderOnlyDataZoom(theme, zoom[0], zoom[1], 16, 4),
      series: [],
    }),
    [endSec, zoom, theme]
  );
  return <EChart option={option} height={48} onEvents={onEvents} />;
}
