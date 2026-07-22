/*
 * Horizontal stacked phase bars shared by the phase breakdown and the per-proof timeline. Share mode
 * normalizes every row to full width so proportions align, time mode keeps real durations on a seconds
 * axis with transparent spacers positioning each bar. A click toggles segment labels between share and
 * seconds.
 */

import { useMemo, useState } from 'react';
import type { EChartsCoreOption } from 'echarts/core';
import type {
  CustomSeriesRenderItemAPI,
  CustomSeriesRenderItemParams,
  CustomSeriesRenderItemReturn,
} from 'echarts';
import { EChart } from '@/components/charts/EChart';
import { ColorDot } from '@/components/common/ColorDot';
import { namedAxis, TRACE_CURSOR_DASH_ARRAY, TRACE_CURSOR_DASH_WIDTH, type AxisTooltipParam } from '@/utils/chartHelpers';
import { contrastText } from '@/utils/color';

export interface BarSegment {
  key: string;
  label: string;
  color: string;
  seconds: number;
  // Spacer holding the gap between two timed segments so the next bar starts at its real time.
  transparent?: boolean;
}

export interface BarRow {
  label: string;
  segments: BarSegment[];
  // Marks a row whose node took no part. Drawn as a hatched ghost band with a centered label instead of
  // phase segments, the data-viz convention for absent data.
  absent?: boolean;
  absentLabel?: string;
}

// Point marker on one row at an exact second, such as a crash moment. Rides on its own custom series so
// it sits over the bars without disturbing the stack, drawn as a dashed vertical line across the row
// with its label above.
export interface BarMarker {
  // Index of the row the marker belongs to, matching the rows array.
  row: number;
  seconds: number;
  label: string;
  color: string;
}

// Segments narrower than this share of the axis print no label because the bar cannot hold one without
// overlapping. The exact value stays in the hover tooltip.
const LABEL_MIN_FRACTION = 0.05;

interface Datum {
  value: number;
  sec: number;
  frac: number;
  transparent: boolean;
}

interface StackedPhaseBarsProps {
  rows: BarRow[];
  mode: 'share' | 'time';
  // Point markers on specific rows, meaningful only in time mode where the axis is seconds.
  markers?: BarMarker[];
  // A full-height dashed cursor line at this second, meaningful only in time mode. Drives the
  // log-console hover that points at the trace moment a hovered line was logged.
  cursorSec?: number | null;
  rowHeight?: number;
  height?: number;
}

export function StackedPhaseBars({ rows, mode, markers, cursorSec, rowHeight = 26, height }: StackedPhaseBarsProps) {
  const [showSeconds, setShowSeconds] = useState(false);

  // The base option without the hovered-log cursor, memoized on the inputs that shape the bars, the
  // absent bands, the crash and cancel markers, and the axes. A cursor hover changes only cursorSec, so
  // holding this base lets the cursor series recombine below without rebuilding every bar and marker
  // series object on each hovered log line.
  const base = useMemo(() => {
    const labels = rows.map(r => r.label);
    // A blank-label spacer row would catch the axis-pointer shadow, so the chart drops the highlight band
    // whenever a spacer is present, leaving the separator row inert.
    const hasSpacer = rows.some(r => !r.label);
    // Slot definitions come from the first row because every row carries the same ordered segments.
    const slots = rows[0]?.segments ?? [];
    const activeTotal = (r: BarRow): number => r.segments.reduce((sum, s) => sum + (s.transparent ? 0 : s.seconds), 0) || 1;
    const fullTotal = (r: BarRow): number => r.segments.reduce((sum, s) => sum + s.seconds, 0);
    // The axis must reach the latest of the bar ends and any marker so a crash marker past the last
    // partial phase lands on the chart not off its right edge.
    const markerSecs = mode === 'time' ? (markers ?? []).map(m => m.seconds) : [];
    const axisMax = mode === 'share' ? 1 : Math.max(0.1, ...rows.map(fullTotal), ...markerSecs);

    const series = slots.map((slot, i) => ({
      name: slot.label,
      type: 'bar' as const,
      stack: 'phase',
      itemStyle: { color: slot.transparent ? 'transparent' : slot.color },
      label: {
        show: !slot.transparent,
        color: contrastText(slot.transparent ? '#000000' : slot.color),
        fontSize: 11,
        formatter: (p: { value: number; data: Datum }) => {
          if (p.data.transparent) return '';
          const widthFrac = mode === 'share' ? p.value : p.value / axisMax;
          if (widthFrac < LABEL_MIN_FRACTION) return '';
          return showSeconds ? `${p.data.sec.toFixed(2)}s` : `${Math.round(p.data.frac * 100)}%`;
        },
      },
      data: rows.map(r => {
        const s = r.segments[i];
        const sec = s?.seconds ?? 0;
        const frac = sec / activeTotal(r);
        return { value: mode === 'share' ? frac : sec, sec, frac, transparent: !!s?.transparent };
      }),
    }));

    // A node that took no part draws as a hatched ghost band spanning the row. It rides in the phase
    // stack at full width, so an absent row (all-zero segments) collapses to just this band.
    const absentSeries =
      mode === 'time' && rows.some(r => r.absent)
        ? [
            {
              name: '__absent__',
              type: 'bar' as const,
              stack: 'phase',
              silent: true,
              z: 1,
              itemStyle: {
                color: 'rgba(127,127,127,0.10)',
                decal: { color: 'rgba(150,150,150,0.45)', dashArrayX: [1, 0], dashArrayY: [3, 4], rotation: -Math.PI / 4 },
              },
              label: {
                show: true,
                position: 'inside' as const,
                color: 'rgba(180,180,180,0.95)',
                fontSize: 11,
                fontStyle: 'italic' as const,
                formatter: (p: { value: number; dataIndex: number }) =>
                  p.value > 0 ? rows[p.dataIndex]?.absentLabel ?? 'did not participate' : '',
              },
              data: rows.map(r => (r.absent ? axisMax : 0)),
            },
          ]
        : [];

    // Each marker is a dashed vertical line across its row at the exact second, labelled above. Rides on
    // a silent custom series so it overlays the bars without joining the stack or tooltip.
    const shownMarkers = mode === 'time' && markers ? markers.filter(m => labels[m.row] != null) : [];
    const markerSeries = shownMarkers.length
      ? [
          {
            type: 'custom' as const,
            z: 6,
            silent: true,
            encode: { x: 0, y: 1 },
            data: shownMarkers.map(m => [m.seconds, m.row]),
            renderItem: (
              params: CustomSeriesRenderItemParams,
              api: CustomSeriesRenderItemAPI
            ): CustomSeriesRenderItemReturn => {
              const marker = shownMarkers[params.dataIndex];
              if (!marker) return { type: 'group', children: [] };
              const [x, y] = api.coord([api.value(0) as number, api.value(1) as number]) as [number, number];
              const band = (api.size!([0, 1]) as number[])[1]!;
              // The line spans the bar centered on it and the label sits in the gap above. The label is
              // clamped below the band top so the first row's label is not clipped at the chart edge.
              const barHalf = band * 0.4;
              const labelY = Math.max(y - band * 0.53, 7);
              return {
                type: 'group',
                children: [
                  {
                    type: 'line',
                    shape: { x1: x, y1: y - barHalf, x2: x, y2: y + barHalf },
                    style: { stroke: marker.color, lineWidth: TRACE_CURSOR_DASH_WIDTH, lineDash: TRACE_CURSOR_DASH_ARRAY },
                  },
                  {
                    type: 'text',
                    style: {
                      text: marker.label,
                      x,
                      y: labelY,
                      fill: marker.color,
                      fontSize: 10,
                      fontWeight: 600,
                      align: 'center',
                      verticalAlign: 'middle',
                    },
                  },
                ],
              };
            },
          },
        ]
      : [];

    return {
      // Animation off so a cursor hover, which rebuilds the option under the base notMerge render, does
      // not replay the bar grow animation on every hovered log line.
      animation: false,
      grid: { left: 8, right: 24, top: 10, bottom: 40, containLabel: true },
      // Phase legend is the ColorDot row below, so the built-in one the base theme adds is hidden.
      legend: { show: false },
      xAxis:
        mode === 'share'
          ? { type: 'value', min: 0, max: 1, axisLabel: { formatter: (v: number) => `${Math.round(v * 100)}%` }, ...namedAxis('Phase share', 28) }
          : {
              type: 'value',
              min: 0,
              max: axisMax,
              // Round the labels to millisecond precision and append the unit, so the axis maximum reads
              // as 24.603 s rather than a raw float like 24.60299999.
              axisLabel: { formatter: (v: number) => `${+v.toFixed(3)} s` },
              ...namedAxis('Seconds', 28),
            },
      yAxis: { type: 'category', inverse: true, data: labels, axisLabel: { fontSize: 12 } },
      tooltip: {
        trigger: 'axis',
        axisPointer: { type: hasSpacer ? 'none' : 'shadow' },
        formatter: (params: unknown) => {
          const arr = params as Array<AxisTooltipParam<number, Datum>>;
          const head = arr[0]?.axisValue ?? '';
          // A blank label marks a spacer row, which carries no values worth a tooltip.
          if (!head) return '';
          // Rows laid out as a borderless table so seconds and share line up in their own right-aligned
          // columns rather than trailing each variable-width phase name.
          const cells = arr
            .filter(p => p.data && typeof p.data.sec === 'number' && !p.data.transparent)
            .map(
              p =>
                `<tr><td>${p.marker}${p.seriesName}</td><td style="text-align:right;padding-left:14px;white-space:nowrap">${p.data.sec.toFixed(2)} s</td><td style="text-align:right;padding-left:10px;white-space:nowrap">${Math.round(p.data.frac * 100)}%</td></tr>`
            )
            .join('');
          return `${head}<table style="border-collapse:collapse;margin-top:3px">${cells}</table>`;
        },
      },
      series: [...series, ...absentSeries, ...markerSeries],
    };
  }, [rows, mode, markers, showSeconds]);

  // A bright full-height dashed cursor at the hovered second, drawn on a silent custom series so it
  // spans the whole grid without a markLine component or joining the stack, with a time badge at the
  // bottom on the seconds axis. Drawn only when a finite cursor second is supplied. Memoized on cursorSec
  // alone so a hover rebuilds only this series, not the bars, absent bands, and markers held in the base.
  const cursorSeries = useMemo(
    () =>
      mode === 'time' && cursorSec != null && Number.isFinite(cursorSec)
        ? [
            {
              type: 'custom' as const,
              z: 7,
              silent: true,
              data: [[cursorSec, 0]],
              renderItem: (
                params: CustomSeriesRenderItemParams,
                api: CustomSeriesRenderItemAPI
              ): CustomSeriesRenderItemReturn => {
                const sec = api.value(0) as number;
                const x = (api.coord([sec, 0]) as [number, number])[0];
                const grid = params.coordSys as unknown as { x: number; y: number; width: number; height: number };
                const bottom = grid.y + grid.height;
                // The badge reads the cursor's seconds value to millisecond precision at the bottom,
                // clamped inside the grid so it never spills past either axis edge.
                const label = `${sec.toFixed(3)}s`;
                const width = label.length * 6.4 + 10;
                const badgeX = Math.min(Math.max(x - width / 2, grid.x), grid.x + grid.width - width);
                const badgeY = bottom + 5;
                const height = 16;
                return {
                  type: 'group',
                  children: [
                    {
                      type: 'line',
                      shape: { x1: x, y1: grid.y, x2: x, y2: bottom },
                      style: { stroke: '#f5f5f5', lineWidth: TRACE_CURSOR_DASH_WIDTH, lineDash: TRACE_CURSOR_DASH_ARRAY },
                    },
                    {
                      type: 'rect',
                      shape: { x: badgeX, y: badgeY, width, height, r: 3 },
                      style: { fill: '#f5f5f5' },
                    },
                    {
                      type: 'text',
                      style: {
                        text: label,
                        x: badgeX + width / 2,
                        y: badgeY + height / 2,
                        fill: '#1a1a1a',
                        fontSize: 10,
                        fontWeight: 'bold',
                        align: 'center',
                        verticalAlign: 'middle',
                      },
                    },
                  ],
                };
              },
            },
          ]
        : [],
    [mode, cursorSec]
  );

  // The base recombined with the cursor series. The cursor rides last so it overlays every other series,
  // matching the prior single-pass build order.
  const option = useMemo<EChartsCoreOption>(
    () => ({ ...base, series: [...(base.series as unknown[]), ...cursorSeries] }),
    [base, cursorSeries]
  );

  const legend = (rows[0]?.segments ?? []).filter(s => !s.transparent);
  return (
    <>
      <div className="mb-2 flex flex-wrap items-center gap-x-4 gap-y-1 px-1 text-xs text-muted">
        {legend.map(s => (
          <ColorDot key={s.key} color={s.color} label={s.label} />
        ))}
        <span className="text-faint">Click a bar to switch percent and seconds.</span>
      </div>
      <EChart
        option={option}
        height={height ?? Math.max(180, rows.length * rowHeight + 64)}
        onEvents={{ click: () => setShowSeconds(s => !s) }}
      />
    </>
  );
}
