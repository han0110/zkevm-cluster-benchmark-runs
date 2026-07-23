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
import { apportionPercents, placedSegmentPieces, type Span } from '@/utils/phaseTimings';

export interface BarSegment {
  key: string;
  label: string;
  color: string;
  seconds: number;
  // Spacer holding the gap between two timed segments so the next bar starts at its real time.
  transparent?: boolean;
  // Absolute placement in axis units for an overlap phase, drawn full height on the overlay series
  // outside the stack so concurrent segments share the row, any interval two of them cover at once
  // filled with the earlier phase's color under stripes of the later's. `frac` is the share the
  // per-phase label reports, required unless the chart labels per piece.
  placed?: { start: number; width: number; frac?: number };
  // Draws the placed bar as the hatched ghost band, the breakdown Rest filler that closes a row to full
  // width, its label toggling share and seconds like a phase. Set only on an overlap-preset breakdown row.
  hatched?: boolean;
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

// Image pattern accepted as a zrender graphic fill.
interface StripeFill {
  image: HTMLCanvasElement;
  repeat: 'repeat';
}

// Canvas tile filling the interval two overlap phases cover at once, the first phase's color as the
// solid base under thin 45 degree diagonal stripes of the second's, cached per pair. The intersection
// then meets the first phase's solid piece with no visible boundary and enters the second's as a
// texture change.
const stripeFills = new Map<string, StripeFill>();
function stripeFill(firstColor: string, secondColor: string): StripeFill {
  const key = `${firstColor}|${secondColor}`;
  const cached = stripeFills.get(key);
  if (cached) return cached;
  const size = 12;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext('2d')!;
  context.fillStyle = firstColor;
  context.fillRect(0, 0, size, size);
  // Stroking the x + y = c diagonals at this width paints the second color exactly where (x + y) mod
  // size falls in a third of the period, thin seamless stripes over the solid base.
  context.strokeStyle = secondColor;
  context.lineWidth = size / (3 * Math.SQRT2);
  context.beginPath();
  for (const diagonal of [size / 4, (5 * size) / 4, (9 * size) / 4]) {
    context.moveTo(-size, diagonal + size);
    context.lineTo(diagonal + size, -size);
  }
  context.stroke();
  const fill: StripeFill = { image: canvas, repeat: 'repeat' };
  stripeFills.set(key, fill);
  return fill;
}

// Canvas tile matching the did-not-participate ghost band, a faint grey base under thin diagonal grey
// stripes, cached in its solid and emphasized shades so a hovered Rest band lifts like a phase bar.
function hatchFill(emphasis: boolean): StripeFill {
  const key = emphasis ? '__hatch_emphasis__' : '__hatch__';
  const cached = stripeFills.get(key);
  if (cached) return cached;
  const size = 12;
  const canvas = document.createElement('canvas');
  canvas.width = size;
  canvas.height = size;
  const context = canvas.getContext('2d')!;
  context.fillStyle = emphasis ? 'rgba(127,127,127,0.18)' : 'rgba(127,127,127,0.10)';
  context.fillRect(0, 0, size, size);
  context.strokeStyle = emphasis ? 'rgba(170,170,170,0.6)' : 'rgba(150,150,150,0.45)';
  context.lineWidth = 1;
  context.beginPath();
  for (const diagonal of [size / 4, (5 * size) / 4, (9 * size) / 4]) {
    context.moveTo(-size, diagonal + size);
    context.lineTo(diagonal + size, -size);
  }
  context.stroke();
  const fill: StripeFill = { image: canvas, repeat: 'repeat' };
  stripeFills.set(key, fill);
  return fill;
}

// Brightens a hex color the way ECharts lifts a series fill on hover, each channel scaled by 1.1 and
// clamped, so a placed piece emphasizes to the shade a stacked bar segment does.
function liftHex(hex: string): string {
  const match = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!match) return hex;
  const channel = (pair: string): string =>
    Math.min(255, Math.floor(parseInt(pair, 16) * 1.1))
      .toString(16)
      .padStart(2, '0');
  return `#${channel(match[1]!)}${channel(match[2]!)}${channel(match[3]!)}`;
}

// Round tooltip marker matching the ECharts axis-tooltip dot, taking any CSS background so the
// striped intersection rows carry a striped dot.
const markerDot = (background: string): string =>
  `<span style="display:inline-block;margin-right:4px;border-radius:10px;width:10px;height:10px;background:${background};"></span>`;

// CSS twin of the stripe fill for tooltip markers, the first color solid under thin stripes of the second.
const stripedMarker = (firstColor: string, secondColor: string): string =>
  markerDot(`repeating-linear-gradient(45deg,${firstColor},${firstColor} 3px,${secondColor} 3px,${secondColor} 4.5px)`);

interface Datum {
  value: number;
  sec: number;
  frac: number;
  transparent: boolean;
}

interface StackedPhaseBarsProps {
  rows: BarRow[];
  mode: 'share' | 'time';
  // Proving seconds behind the trace percent labels, set only for the block trace of an overlap
  // preset. When set the chart labels per piece, every solid or striped placed piece and every
  // stacked segment reporting its duration over this base so a row's percents sum toward 100, and
  // the tooltip lists the same pieces. Null marks a block reporting no proving time, so piece labels
  // read in seconds. When absent each placed phase labels once with its `frac`.
  pieceLabelBaseSec?: number | null;
  // Point markers on specific rows, meaningful only in time mode where the axis is seconds.
  markers?: BarMarker[];
  // A full-height dashed cursor line at this second, meaningful only in time mode. Drives the
  // log-console hover that points at the trace moment a hovered line was logged.
  cursorSec?: number | null;
  rowHeight?: number;
  height?: number;
}

export function StackedPhaseBars({ rows, mode, pieceLabelBaseSec, markers, cursorSec, rowHeight = 26, height }: StackedPhaseBarsProps) {
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
    // A placed segment leaves the stack, so it counts toward the axis by its own end not the row sum.
    const fullTotal = (r: BarRow): number => r.segments.reduce((sum, s) => sum + (s.placed ? 0 : s.seconds), 0);
    const placedEnds = rows.flatMap(r => r.segments.flatMap(s => (s.placed ? [s.placed.start + s.placed.width] : [])));
    // The axis must reach the latest of the bar ends, placed ends, and any marker so a crash marker past
    // the last partial phase lands on the chart not off its right edge.
    const markerSecs = mode === 'time' ? (markers ?? []).map(m => m.seconds) : [];
    const axisMax = mode === 'share' ? 1 : Math.max(0.1, ...rows.map(fullTotal), ...placedEnds, ...markerSecs);

    // Piece labeling engages whenever a base is supplied, the block-trace semantics. A supplied but
    // null base marks a block reporting no proving time, so its percent labels give way to seconds.
    const pieceMode = pieceLabelBaseSec !== undefined;
    const secondsOnly = pieceMode && !pieceLabelBaseSec;

    const series = slots.map((slot, i) => ({
      name: slot.label,
      type: 'bar' as const,
      stack: 'phase',
      itemStyle: { color: slot.transparent ? 'transparent' : slot.color },
      label: {
        show: !slot.transparent,
        color: contrastText(slot.transparent ? '#000000' : slot.color),
        fontSize: 11,
        formatter: (p: { value: number; data: Datum; dataIndex: number }) => {
          if (p.data.transparent) return '';
          const widthFrac = mode === 'share' ? p.value : p.value / axisMax;
          if (widthFrac < LABEL_MIN_FRACTION) return '';
          if (showSeconds || secondsOnly) return `${p.data.sec.toFixed(2)}s`;
          // A piece percent under piece labeling is the row's apportioned integer so the bar matches the
          // tooltip, while a non-overlap stacked row keeps its independent per-phase rounding.
          return pieceMode ? `${piecePercent(p.dataIndex, slot.key)}%` : `${Math.round(p.data.frac * 100)}%`;
        },
      },
      data: rows.map(r => {
        const s = r.segments[i];
        const sec = s?.seconds ?? 0;
        const frac = pieceMode ? (pieceLabelBaseSec ? sec / pieceLabelBaseSec : 0) : s?.placed?.frac ?? sec / activeTotal(r);
        // A placed segment contributes zero to the stack, keeping its real figures for the per-phase
        // tooltip while the overlay series draws its bar at the absolute position.
        return { value: s?.placed ? 0 : mode === 'share' ? frac : sec, sec, frac, transparent: !!s?.transparent };
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

    // Placed segments draw as absolutely positioned full-height bars on a custom series over the stack,
    // solid where one phase runs alone and striped where two run at once. Their figures stay in the
    // per-phase tooltip through their zero-width stack entries, and the series stays clickable so the
    // percent-and-seconds toggle works where the stack holds no visible bar.
    const placedSegments = rows.flatMap((r, row) =>
      r.segments.flatMap(s => {
        if (!s.placed || s.placed.width <= 0) return [];
        return [{ row, key: s.key, label: s.label, color: s.color, sec: s.seconds, frac: s.placed.frac, start: s.placed.start, end: s.placed.start + s.placed.width, hatched: !!s.hatched }];
      })
    );
    // Each segment keeps the parts no row sibling covers as solid pieces and takes a striped piece for
    // its intersection with each earlier sibling, so every interval is painted exactly once. The
    // per-phase label sits on the widest solid piece, falling back to the whole span when siblings
    // cover it all.
    const placedDraws = placedSegments.map(segment => {
      const rowSegments = placedSegments.filter(p => p.row === segment.row);
      const pieces = placedSegmentPieces(rowSegments, rowSegments.indexOf(segment));
      const striped = pieces.striped.map(({ span, first }) => ({ span, first, fill: stripeFill(first.color, segment.color) }));
      const label = pieces.solid.length
        ? pieces.solid.reduce((widest, piece) => (piece.end - piece.start > widest.end - widest.start ? piece : widest))
        : { start: segment.start, end: segment.end };
      return { solid: pieces.solid, striped, label };
    });
    // Under piece labels the tooltip lists the same pieces the bars label, each placed phase reduced
    // to its solid remainder plus one row per striped intersection named for both phases, so the
    // listed figures partition the row and sum toward the whole like the labels.
    const pieceTooltips = pieceMode
      ? rows.map((r, rowIndex) => {
          const rowPlaced = placedSegments.flatMap((p, i) => (p.row === rowIndex ? [{ segment: p, draw: placedDraws[i]! }] : []));
          return r.segments.flatMap(s => {
            if (s.transparent) return [];
            // A share-normalized placement reads a piece's seconds from its width through the phase's own
            // seconds-per-fraction scale and its share from the row base, so the listed figures partition
            // the row like the labels. A seconds-axis trace placement keeps its width as seconds against a
            // null-safe base. Each piece carries a stable key so the bar labels and this tooltip read the
            // same apportioned integer.
            const secPerFrac = s.placed?.frac ? s.seconds / s.placed.frac : 1;
            const entry = (key: string, marker: string, name: string, width: number) => ({
              key,
              marker,
              name,
              sec: width * secPerFrac,
              share: pieceLabelBaseSec != null ? width / pieceLabelBaseSec : null,
            });
            const placed = rowPlaced.find(p => p.segment.key === s.key);
            if (!placed) return [entry(s.key, markerDot(s.color), s.label, s.seconds)];
            return [
              ...placed.draw.striped.map((piece, i) =>
                entry(`${s.key}::${piece.first.key}::${i}`, stripedMarker(piece.first.color, s.color), `${piece.first.label} + ${s.label}`, piece.span.end - piece.span.start)
              ),
              entry(`${s.key}::solid`, markerDot(s.color), s.label, placed.draw.solid.reduce((sum, piece) => sum + (piece.end - piece.start), 0)),
            ];
          });
        })
      : [];
    // Per row, apportion the labelable pieces' shares to whole percents so the bar labels and the tooltip
    // read one integer per piece and a partitioned row's percents sum to 100. Seconds-only rows carry no
    // percentages to apportion.
    const rowPiecePercents = pieceMode && !secondsOnly
      ? pieceTooltips.map(pieces => {
          const percents = apportionPercents(pieces.map(p => p.share ?? 0));
          return new Map(pieces.map((p, i) => [p.key, percents[i]!]));
        })
      : [];
    const piecePercent = (rowIndex: number, key: string): number => rowPiecePercents[rowIndex]?.get(key) ?? 0;
    const placedSeries = placedSegments.length
      ? [
          {
            type: 'custom' as const,
            z: 3,
            encode: { x: 0, y: 1 },
            data: placedSegments.map(p => [p.start, p.row]),
            renderItem: (
              params: CustomSeriesRenderItemParams,
              api: CustomSeriesRenderItemAPI
            ): CustomSeriesRenderItemReturn => {
              const segment = placedSegments[params.dataIndex];
              const draw = placedDraws[params.dataIndex];
              if (!segment || !draw) return { type: 'group', children: [] };
              const y = (api.coord([segment.start, segment.row]) as [number, number])[1];
              // The stacked bar series leave sizing to the default bar layout, one stack column on
              // the category band, so the same one-column layout gives the exact stacked-bar height
              // and vertical offset.
              const layout = api.barLayout({ count: 1 })![0]!;
              const top = y + layout.offset;
              const barHeight = layout.width;
              const xAt = (value: number): number => (api.coord([value, segment.row]) as [number, number])[0];
              // Piece edges snap to device pixels, so abutting solid and striped rects meet on the
              // same pixel without an antialiased seam.
              const devicePixels = window.devicePixelRatio || 1;
              const snap = (value: number): number => Math.round(value * devicePixels) / devicePixels;
              // Each piece carries an emphasis fill so hovering the placed bar, solid or striped,
              // lifts it to the same brighter shade a stacked bar segment gets on hover.
              const rect = (span: Span, fill: string | StripeFill, emphasisFill: string | StripeFill) => {
                const left = snap(xAt(span.start));
                const right = snap(xAt(span.end));
                return {
                  type: 'rect' as const,
                  shape: { x: left, y: top, width: Math.max(0, right - left), height: barHeight },
                  style: { fill },
                  emphasis: { style: { fill: emphasisFill } },
                };
              };
              const fits = (span: Span): boolean => (span.end - span.start) / axisMax >= LABEL_MIN_FRACTION;
              const textAt = (text: string, span: Span, onColor: string) => ({
                type: 'text' as const,
                style: {
                  text,
                  x: (xAt(span.start) + xAt(span.end)) / 2,
                  y,
                  fill: contrastText(onColor),
                  fontSize: 11,
                  align: 'center' as const,
                  verticalAlign: 'middle' as const,
                },
              });
              // The Rest filler reports its share or seconds like a phase, in a muted fill legible on the
              // faint hatch.
              const hatchedLabel = (span: Span, text: string) => ({
                type: 'text' as const,
                style: {
                  text,
                  x: (xAt(span.start) + xAt(span.end)) / 2,
                  y,
                  fill: 'rgba(200,200,200,0.95)',
                  fontSize: 11,
                  align: 'center' as const,
                  verticalAlign: 'middle' as const,
                },
              });
              // A piece reports its own seconds through the placement's per-fraction scale, and its percent
              // is the row's apportioned integer so the bar and the tooltip read the same value. A
              // share-normalized placement carries its seconds through the scale, a seconds-axis trace
              // placement keeps its width as seconds.
              const secPerFrac = segment.frac != null && segment.frac > 0 ? segment.sec / segment.frac : 1;
              const pieceSeconds = (span: Span): string => `${((span.end - span.start) * secPerFrac).toFixed(2)}s`;
              const solidKey = `${segment.key}::solid`;
              const texts = segment.hatched
                ? fits(draw.label)
                  ? [hatchedLabel(draw.label, showSeconds ? `${segment.sec.toFixed(2)}s` : `${piecePercent(segment.row, solidKey)}%`)]
                  : []
                : showSeconds || secondsOnly
                ? [
                    ...draw.solid.filter(fits).map(piece => textAt(pieceSeconds(piece), piece, segment.color)),
                    ...draw.striped.filter(piece => fits(piece.span)).map(piece => textAt(pieceSeconds(piece.span), piece.span, piece.first.color)),
                  ]
                : [
                    // A hidden small piece still prints no bar label, so the visible labels can total under
                    // 100 while the full apportioned set and the tooltip sum to 100.
                    ...(draw.solid.length > 0 && fits(draw.label)
                      ? [textAt(`${piecePercent(segment.row, solidKey)}%`, draw.label, segment.color)]
                      : []),
                    ...draw.striped
                      .map((piece, i) => ({ piece, i }))
                      .filter(({ piece }) => fits(piece.span))
                      .map(({ piece, i }) =>
                        textAt(`${piecePercent(segment.row, `${segment.key}::${piece.first.key}::${i}`)}%`, piece.span, piece.first.color)
                      ),
                  ];
              const emphasisColor = liftHex(segment.color);
              const solidFill = segment.hatched ? hatchFill(false) : segment.color;
              const solidEmphasis = segment.hatched ? hatchFill(true) : emphasisColor;
              return {
                type: 'group',
                children: [
                  ...draw.solid.map(piece => rect(piece, solidFill, solidEmphasis)),
                  ...draw.striped.map(piece =>
                    rect(piece.span, piece.fill, stripeFill(liftHex(piece.first.color), emphasisColor))
                  ),
                  ...texts,
                ],
              };
            },
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
          const cell = (marker: string, name: string, sec: number, share: string) =>
            `<tr><td>${marker}${name}</td><td style="text-align:right;padding-left:14px;white-space:nowrap">${sec.toFixed(2)} s</td><td style="text-align:right;padding-left:10px;white-space:nowrap">${share}</td></tr>`;
          const dataIndex = arr[0]?.dataIndex ?? -1;
          const cells = pieceMode
            ? (pieceTooltips[dataIndex] ?? [])
                .map(piece => cell(piece.marker, piece.name, piece.sec, piece.share != null ? `${piecePercent(dataIndex, piece.key)}%` : ''))
                .join('')
            : arr
                .filter(p => p.data && typeof p.data.sec === 'number' && !p.data.transparent)
                .map(p => cell(p.marker, p.seriesName, p.data.sec, `${Math.round(p.data.frac * 100)}%`))
                .join('');
          return `${head}<table style="border-collapse:collapse;margin-top:3px">${cells}</table>`;
        },
      },
      series: [...series, ...absentSeries, ...placedSeries, ...markerSeries],
    };
  }, [rows, mode, pieceLabelBaseSec, markers, showSeconds]);

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

  const legend = (rows[0]?.segments ?? []).filter(s => !s.transparent && !s.hatched);
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
