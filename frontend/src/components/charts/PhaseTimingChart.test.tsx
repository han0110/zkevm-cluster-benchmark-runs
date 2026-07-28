import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { EChartsCoreOption } from 'echarts/core';

// The canvas renderer has no jsdom backing, so the wrapper is stubbed and the built option captured
// instead. The option is the chart's whole contract with echarts, so asserting it covers the series and
// axis the reader sees.
const captured: { option: EChartsCoreOption | null } = { option: null };
vi.mock('@/components/charts/EChart', () => ({
  EChart: ({ option }: { option: EChartsCoreOption }) => {
    captured.option = option;
    return <div data-testid="chart" />;
  },
}));

import { PhaseTimingChart } from '@/components/charts/PhaseTimingChart';
import { fixture } from '@/test/fixture';
import { openvmBenchmark } from '@/test/openvmFixture';
import { buildPhaseRegistry } from '@/utils/phases';
import type { PhaseRegistry } from '@/utils/phases';

const zisk = buildPhaseRegistry(fixture);
const openvm = buildPhaseRegistry(openvmBenchmark([]));

// The named series of the built option, dropping the unnamed threshold marker series.
function seriesNames(): string[] {
  const series = (captured.option as { series: { name?: string }[] }).series;
  return series.flatMap(s => (s.name == null ? [] : [s.name]));
}

function renderChart(registry: PhaseRegistry, totalOnly: boolean) {
  captured.option = null;
  const values = Object.fromEntries(registry.list.map(p => [p.name, [1, 2]]));
  return render(
    <PhaseTimingChart
      labels={['0001', '0002']}
      values={values}
      registry={registry}
      total={[4, 6]}
      totalOnly={totalOnly}
    />
  );
}

describe('PhaseTimingChart', () => {
  it('draws one line per phase beside the total, with a legend to isolate them', () => {
    renderChart(zisk, false);
    expect(seriesNames()).toEqual(['Total', ...zisk.list.map(p => p.label)]);
    // One legend entry per series, each a button that isolates its own line.
    expect(screen.getAllByRole('button').length).toBe(zisk.list.length + 1);
  });

  it('draws the total alone and hides the legend for an overlap preset', () => {
    renderChart(openvm, true);
    expect(seriesNames()).toEqual(['Total']);
    // A lone series has nothing to select between, so no legend renders at all.
    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

  it('keeps the axis ceiling over the total and the reference thresholds in total-only mode', () => {
    renderChart(openvm, true);
    const { yAxis, dataZoom } = captured.option as { yAxis: { max: number }; dataZoom: unknown[] };
    // The 12 s reference line still pins the ceiling, so dropping the phase lines never rescales it.
    expect(yAxis.max).toBeGreaterThanOrEqual(Math.ceil(12 * 1.05));
    // The zoom slider survives, since the panel is still scrolled across the run.
    expect(dataZoom.length).toBeGreaterThan(0);
  });
});
