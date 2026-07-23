/*
 * Scatter of gas used vs proving time with a least-squares trend line. Points are colored by their
 * relative deviation from the trend so a block that proves slower or faster than its gas predicts stands
 * out from the run.
 */

import { useMemo } from 'react';
import type { EChartsCoreOption } from 'echarts/core';
import { EChart } from '@/components/charts/EChart';
import { useThemeColors } from '@/hooks/useThemeColors';
import { namedAxis, emptyChartOption, type ItemTooltipParam } from '@/utils/chartHelpers';
import { msToSec } from '@/utils/format';
import { blockLabel } from '@/utils/phases';
import { linearFit, predict, relativeResidual, type LinearFit } from '@/utils/trendFit';
import type { Block } from '@/types/benchmark';

interface ScatterLineProps {
  blocks: Block[];
  height?: number;
}

// Symmetric residual bound in fraction of predicted time. The diverging color scale saturates here so an
// extreme outlier reads full-warm or full-cool without washing out the near-line mid-range.
const RESIDUAL_CLAMP = 0.5;

// Signed fixed-digit number with an explicit plus so a deviation reads as an offset from the trend.
const signed = (value: number, digits: number): string => `${value >= 0 ? '+' : '-'}${Math.abs(value).toFixed(digits)}`;

// Tooltip line stating how far a point runs from the trend, in seconds and as a percent of predicted.
const trendNote = (fit: LinearFit, [gasM, sec]: readonly number[]): string => {
  const predicted = predict(fit, gasM as number);
  const deltaSec = (sec as number) - predicted;
  const percent = predicted > 0 ? (deltaSec / predicted) * 100 : 0;
  return `${signed(deltaSec, 2)} s vs trend (${signed(percent, 0)}%)`;
};

export function ScatterLine({ blocks, height = 360 }: ScatterLineProps) {
  const colors = useThemeColors();

  const option = useMemo<EChartsCoreOption>(() => {
    const rows = blocks.filter(p => p.status === 'success' && p.gas_used != null);
    const labels = rows.map(blockLabel);
    const points: Array<[number, number]> = rows.map(p => [(p.gas_used as number) / 1e6, msToSec(p.proving_ms ?? 0)]);
    if (points.length === 0) return emptyChartOption('value');
    const fit = linearFit(points);
    // Dimensions are gas (M), proving time (s), and the relative residual that drives the color.
    const data: Array<[number, number, number]> = points.map(pt => [pt[0], pt[1], fit ? relativeResidual(fit, pt) : 0]);
    const xs = points.map(pt => pt[0]);
    const x0 = Math.min(...xs);
    const x1 = Math.max(...xs);
    const trend = fit ? [[x0, predict(fit, x0)], [x1, predict(fit, x1)]] : [];
    return {
      grid: { left: 56, right: 72, top: 16, bottom: 48, containLabel: true },
      xAxis: { type: 'value', ...namedAxis('Gas used (M)', 30), min: 0 },
      yAxis: { type: 'value', ...namedAxis('Proving time (s)', 40), min: 0 },
      visualMap: {
        seriesIndex: 0,
        dimension: 2,
        min: -RESIDUAL_CLAMP,
        max: RESIDUAL_CLAMP,
        calculable: true,
        orient: 'vertical',
        right: 0,
        top: 'middle',
        itemWidth: 12,
        itemHeight: 60,
        precision: 2,
        text: [`+${RESIDUAL_CLAMP * 100}% slower`, `-${RESIDUAL_CLAMP * 100}% faster`],
        textStyle: { color: colors.muted, fontSize: 10 },
        // Cool below the trend, muted on it, warm above, so a slower-than-trend block reads warm and a
        // faster one cool.
        inRange: { color: [colors.accent, colors.muted, colors.danger] },
      },
      tooltip: {
        trigger: 'item',
        formatter: (p: ItemTooltipParam<[number, number, number]>) => {
          const base = `${labels[p.dataIndex] ?? ''}<br/>${p.value[0].toFixed(1)} M gas<br/>${p.value[1].toFixed(2)} s`;
          return fit ? `${base}<br/>${trendNote(fit, p.value)}` : base;
        },
      },
      series: [
        {
          type: 'scatter',
          data,
          symbolSize: 9,
          itemStyle: { opacity: 0.8 },
        },
        {
          type: 'line',
          data: trend,
          showSymbol: false,
          silent: true,
          lineStyle: { color: colors.faint, width: 2, type: 'dashed' },
        },
      ],
    };
  }, [blocks, colors]);

  return <EChart option={option} height={height} />;
}
