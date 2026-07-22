/* Shared ECharts option fragments and tooltip-parameter shapes reused across charts. */

import type { ThemeColors } from '@/hooks/useThemeColors';

// Value-axis name centered on the axis with a fixed gap.
export const namedAxis = (name: string, gap: number) => ({
  name,
  nameLocation: 'middle' as const,
  nameGap: gap,
});

// Placeholder option for an empty chart, bare axes and no series so a no-data run renders a clean frame
// instead of throwing. The x-axis type matches the populated chart's.
export const emptyChartOption = (xType: 'category' | 'value') => ({
  xAxis: { type: xType },
  yAxis: { type: 'value' as const },
  series: [],
});

// Coalesce interval in ms for wheel and live slider-drag datazoom events. Caps how often the connected
// telemetry stack and its relay re-render during a gesture, keeping scroll smooth while the slider stays
// live because the handle still tracks the cursor between ticks.
const ZOOM_THROTTLE = 60;

// Order start/end percent so a window is never inverted, guarding an upstream getter that produced a
// degenerate range before echarts bakes it in.
const ordered = (start: number, end: number): [number, number] => (start <= end ? [start, end] : [end, start]);

// Styled range slider where the brand color fills the selection and tints the handles while the
// unselected data preview recedes to the border.
const styledSlider = (c: ThemeColors, height: number, bottom: number) => ({
  type: 'slider' as const,
  height,
  bottom,
  borderColor: c.border,
  backgroundColor: 'transparent',
  fillerColor: hexA(c.primary, 0.18),
  handleStyle: { color: c.primary, borderColor: c.primary },
  moveHandleStyle: { color: c.primary },
  textStyle: { color: c.muted, fontSize: 10 },
  dataBackground: { lineStyle: { color: c.border, opacity: 0.5 }, areaStyle: { color: c.border, opacity: 0.08 } },
  selectedDataBackground: { lineStyle: { color: c.primary, opacity: 0.6 }, areaStyle: { color: c.primary, opacity: 0.08 } },
});

// Inside zoom plus a styled range slider for one self-contained chart. The wheel zooms not pans
// (moveOnMouseMove off). filterMode none keeps off-window points so a line still draws across the edges
// instead of reading as the end of the data. minValueSpan floors the window so a deep zoom never
// collapses. start/end percent are supplied so a chosen window survives a notMerge update.
export const sliderDataZoom = (c: ThemeColors, start = 0, end = 100, minValueSpan = 2, height = 18, bottom = 8) => {
  const [s, e] = ordered(start, end);
  return [
    { type: 'inside' as const, filterMode: 'none' as const, minValueSpan, zoomOnMouseWheel: true, moveOnMouseMove: false, throttle: ZOOM_THROTTLE, start: s, end: e },
    { ...styledSlider(c, height, bottom), filterMode: 'none' as const, minValueSpan, throttle: ZOOM_THROTTLE, start: s, end: e },
  ];
};

// Slider-only zoom for a chart living inside a scrollable container, whose plain wheel must keep
// scrolling that container. An inside wheel zoom stops wheel events from ever bubbling out of the
// canvas, so the slider is the only zoom control here. minSpan floors the window in percent so the
// handles never collapse to a zero-width extent.
export const sliderOnlyDataZoom = (c: ThemeColors, start = 0, end = 100, height = 16, bottom = 6) => {
  const [s, e] = ordered(start, end);
  return [
    { ...styledSlider(c, height, bottom), filterMode: 'none' as const, minSpan: 1, throttle: ZOOM_THROTTLE, start: s, end: e },
  ];
};

// Zoom config for a stack of connected telemetry charts. Every chart carries the same inside and slider
// component ids so echarts.connect, which routes a dataZoom action by component id, broadcasts a drag
// from any panel to every other. start/end percent survive a notMerge update, optional minValueSpan
// matches the phase chart's floor when synced, and filterMode none keeps lines continuous.
export const syncDataZoom = (c: ThemeColors, showSlider: boolean, start = 0, end = 100, minValueSpan?: number) => {
  const floor = minValueSpan == null ? {} : { minValueSpan };
  const [s, e] = ordered(start, end);
  return [
    { type: 'inside' as const, id: 'tel-inside', filterMode: 'none' as const, throttle: ZOOM_THROTTLE, start: s, end: e, ...floor },
    { ...styledSlider(c, 16, 6), id: 'tel-slider', show: showSlider, filterMode: 'none' as const, throttle: ZOOM_THROTTLE, start: s, end: e, ...floor },
  ];
};

// Pull start/end percent from an echarts datazoom event, batched (linked inside and slider) or flat,
// returning null when the payload carries no numeric range.
export function parseDataZoom(p: unknown): { start: number; end: number } | null {
  const b = p as { batch?: Array<{ start?: number; end?: number }>; start?: number; end?: number };
  const z = b.batch?.[0] ?? b;
  return typeof z.start === 'number' && typeof z.end === 'number' ? { start: z.start, end: z.end } : null;
}

// Dash style for the trace-cursor family, the dashed vertical reference lines marking a moment on the
// block trace's time axis, the hovered-log cursor and the crash and cancel markers.
export const TRACE_CURSOR_DASH_WIDTH = 2;
export const TRACE_CURSOR_DASH_ARRAY: [number, number] = [6, 4];

// Apply an alpha channel to a 6-digit hex color, producing the 8-digit form ECharts accepts.
export function hexA(hex: string, alpha: number): string {
  const a = Math.round(Math.max(0, Math.min(1, alpha)) * 255)
    .toString(16)
    .padStart(2, '0');
  return /^#[0-9A-Fa-f]{6}$/.test(hex) ? `${hex}${a}` : hex;
}

// markArea data for the neutral hover highlight band, one area spanning start to end second tinted by
// foreground. The per-chart repaint and the stack-wide painter share this one-area literal so the merged
// option is identical across both call sites.
export const highlightArea = (c: ThemeColors, [start, end]: [number, number]) => [
  [{ xAxis: start, itemStyle: { color: c.foreground, opacity: 0.12 } }, { xAxis: end }],
];

// One entry an axis-trigger tooltip formatter receives, generic over value and data shapes. ECharts
// types these as a loose union, so each chart narrows through this shared shape not an inline cast.
export interface AxisTooltipParam<TValue = number, TData = unknown> {
  axisValue: string;
  dataIndex: number;
  seriesName: string;
  marker: string;
  color: string;
  value: TValue;
  data: TData;
}

// The single param an item-trigger tooltip formatter receives, generic over the point value shape.
export interface ItemTooltipParam<TValue = number[]> {
  seriesName: string;
  marker: string;
  color: string;
  dataIndex: number;
  value: TValue;
}

// Grafana-style legend selection where the full set is the unfiltered state. A modifier click toggles
// one key, a plain click isolates the key, and a plain click on the lone selected key resets to full.
export function grafanaSelect(set: Set<string>, allKeys: string[], key: string, multi: boolean): Set<string> {
  if (multi) {
    const next = new Set(set);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    return next;
  }
  return set.size === 1 && set.has(key) ? new Set(allKeys) : new Set([key]);
}
