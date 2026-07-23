/*
 * Overview page stacking identity, statistics, proving-time distribution, phase breakdown, critical-path
 * phase timing, and the gas-vs-time scatter. Every per-block view reads the benchmark's merged latest
 * fixtures so a patched benchmark shows its whole result, not only the newest run.
 */

import { useMemo } from 'react';
import { useBench } from '@/hooks/useBench';
import { usePersistentState } from '@/hooks/usePersistentState';
import { cx } from '@/utils/cx';
import { FOCUS_RING, PILL, PILL_IDLE } from '@/utils/styles';
import { StatStrip, type StatItem } from '@/components/common/StatStrip';
import { HardwareTable } from '@/components/common/HardwareTable';
import { ChartPanel } from '@/components/common/ChartPanel';
import { ChartSection } from '@/components/common/ChartSection';
import { SectionHeading } from '@/components/common/SectionHeading';
import { ScatterLine } from '@/components/charts/ScatterLine';
import { ProvingTimeHistogram } from '@/components/charts/ProvingTimeHistogram';
import { PhaseBreakdownChart, type PhaseBreakdownRow } from '@/components/charts/PhaseBreakdownChart';
import { PhaseTimingChart } from '@/components/charts/PhaseTimingChart';
import { clusterPhaseSeries, hasOverlapPhases, meanClusterPhases, meanWindowPhases, type PhaseTimingMode } from '@/utils/phaseTimings';
import { provingTimeBuckets, bucketRangeLabel, BUCKET_S } from '@/utils/provingTimeBuckets';
import { buildPhaseRegistry } from '@/utils/phases';
import { latestBlocks } from '@/utils/runs';
import { summarizeProofs } from '@/utils/proofStats';
import { formatCompact, formatMsSeconds, dash, formatDateTime } from '@/utils/format';

// Histogram bin widths in seconds, cycled fine to coarse by the panel's bucket pill.
const HISTOGRAM_BUCKETS_S = [0.1, 0.5, 1];

// Default bin width for a proving-time spread, finer for tighter distributions so the bar count stays
// readable. A spread under 16 s takes 0.1 s bins and a spread under 80 s takes 0.5 s bins. Anything
// wider takes 1 s bins.
const defaultBucketS = (spreadS: number): number => (spreadS < 16 ? 0.1 : spreadS < 80 ? 0.5 : 1);

export function OverviewPage() {
  const data = useBench();
  const { software, hardware } = data;
  const registry = useMemo(() => buildPhaseRegistry(data), [data]);
  // The merged latest fixtures across every run, the set every per-block view and the headline stats
  // summarize, so the overview reflects the whole benchmark not a single run.
  const blocks = useMemo(() => latestBlocks(data), [data]);
  const summary = useMemo(() => summarizeProofs(blocks), [blocks]);
  // The benchmark started when its first run did, the earliest start across the runs.
  const startedAt = useMemo(() => Math.min(...data.runs.map(r => r.started_at)), [data]);

  // Fixed default-width bucketing feeding the phase-breakdown rows, which stay coarse enough to read
  // as a table regardless of the histogram's bin toggle.
  const buckets = provingTimeBuckets(blocks);

  // The histogram bins at its own selectable width, a pill the reader cycles through fine to coarse.
  // The stored value stays null until the reader clicks, so an untouched chart keeps following the
  // spread-driven default across benchmarks instead of freezing the first default it saw. A stale
  // persisted width outside the cycle falls back to the first entry on the next click.
  const provingSpreadS = useMemo(() => {
    const times = blocks.filter(b => b.status === 'success').flatMap(b => (b.proving_ms == null ? [] : [b.proving_ms]));
    return times.length ? (Math.max(...times) - Math.min(...times)) / 1000 : 0;
  }, [blocks]);
  const [storedBucketS, setStoredBucketS] = usePersistentState<number | null>('overview-histogram-bucket-s', null);
  const histBucketS = storedBucketS ?? defaultBucketS(provingSpreadS);
  const histBuckets = provingTimeBuckets(blocks, histBucketS);
  const cycleHistBucketS = () =>
    setStoredBucketS(HISTOGRAM_BUCKETS_S[(HISTOGRAM_BUCKETS_S.indexOf(histBucketS) + 1) % HISTOGRAM_BUCKETS_S.length] ?? BUCKET_S);

  // The per-block phase chart can order blocks by proving time instead of by name, a tab the reader
  // cycles. Sorting by time is ascending, so the slowest block lands on the right.
  const [sortByTime, setSortByTime] = usePersistentState('overview-phase-sort-by-time', false);
  const phaseChartBlocks = useMemo(
    () => (sortByTime ? [...blocks].sort((a, b) => (a.proving_ms ?? 0) - (b.proving_ms ?? 0)) : blocks),
    [blocks, sortByTime]
  );
  const phaseChart = useMemo(() => clusterPhaseSeries(phaseChartBlocks, registry), [phaseChartBlocks, registry]);

  // One breakdown row for every block, then one per non-empty proving-time bucket from fastest to slowest.
  // An overlap preset takes the sum-based window math, whose phases plus a hatched Rest close each row at
  // 100 percent, read by the header's per-node or cluster-total divisor. A presetless cluster keeps the
  // stacked critical-path math and shows no divisor control.
  const overlap = hasOverlapPhases(registry);
  // Per node leads so a single-node phase reads its full mean instead of diluting across every node, a
  // divisor the header's toggle button flips to cluster total.
  const [phaseTimingMode, setPhaseTimingMode] = usePersistentState<PhaseTimingMode>('overview-phase-timing-mode', 'perNode');
  const meanPhases = (bs: typeof blocks) =>
    overlap ? meanWindowPhases(bs, registry, phaseTimingMode) : meanClusterPhases(bs, registry);
  const allBlocks: PhaseBreakdownRow = { label: 'All blocks', ...meanPhases(blocks) };
  const bucketRows: PhaseBreakdownRow[] = buckets.byBucket.flatMap((ps, i) =>
    ps.length ? [{ label: bucketRangeLabel(i, buckets.bucketS), ...meanPhases(ps) }] : []
  );
  // A blank zero-value row sets the All-blocks summary apart from the per-bucket rows below it.
  const spacer: PhaseBreakdownRow = { label: '', phases: allBlocks.phases.map(p => ({ key: p.key, label: p.label, seconds: 0 })), total: 0 };
  const phaseRows: PhaseBreakdownRow[] = bucketRows.length ? [allBlocks, spacer, ...bucketRows] : [allBlocks];

  // An overlap preset stripes one band per consecutive overlap pair, each named in the subtitle from the preset.
  const overlapPhases = registry.list.filter(p => p.overlap);
  const bandLabels = overlapPhases.slice(1).map((p, k) => `${overlapPhases[k]!.label} + ${p.label}`).join(' and ');
  // The lead sentence tracks the divisor. Cluster total reports pooled-time shares, per node reports the
  // per-node means that keep a single-node phase at its full duration.
  const breakdownLead =
    phaseTimingMode === 'perNode'
      ? 'Per-node mean time in each phase, so a single-node phase keeps its full duration.'
      : 'Share of pooled worker-active time across all nodes.';
  const breakdownSubtitle = overlap
    ? `${breakdownLead} Striped bands mark ${bandLabels} overlapping. Rest is time unattributed.`
    : 'Mean share of proving time per phase, with each phase ending when the last node finishes it.';

  const softwareItems: StatItem[] = [
    { label: 'zkVM', value: software.zkvm.name },
    { label: 'zkVM Version', value: software.zkvm.version },
    { label: 'Guest', value: software.guest.name },
    { label: 'Guest Version', value: software.guest.version },
  ];

  const failed = summary.count - summary.success;
  const benchmarkItems: StatItem[] = [
    { label: 'Name', value: data.name || data.id },
    { label: 'Description', value: data.description || '-' },
    { label: 'Benchmark At', value: benchmarkTime(startedAt) },
    { label: 'Proofs', value: `${summary.success}/${summary.count}${failed ? ` (${failed} failed)` : ''}` },
    { label: 'Mean Throughput', value: dash(summary.gasPerSecond, g => `${formatCompact(g)} gas/s`) },
    { label: 'Mean Time', value: formatMsSeconds(summary.meanMs) },
    {
      label: 'Time (P50 / P90 / P95 / P99)',
      value: `${formatMsSeconds(summary.p50Ms)} / ${formatMsSeconds(summary.p90Ms)} / ${formatMsSeconds(summary.p95Ms)} / ${formatMsSeconds(summary.p99Ms)}`,
    },
  ];

  return (
    <div className="flex flex-col gap-6">
      <ChartSection title="Hardware">
        <HardwareTable hardware={hardware} />
      </ChartSection>

      <ChartSection title="Software">
        <StatStrip items={softwareItems} />
      </ChartSection>

      {/* The Benchmark group, a stat strip of identity figures above its chart panels, the panels
          padded apart so the section reads like a Grafana row of panels under one group. */}
      <section className="flex flex-col gap-4">
        <SectionHeading>Benchmark</SectionHeading>
        <StatStrip items={benchmarkItems} />

        <ChartPanel
          title="Proving time distribution"
          action={
            <button type="button" onClick={cycleHistBucketS} className={cx(PILL, FOCUS_RING, PILL_IDLE)}>
              {`${histBucketS} s buckets`}
            </button>
          }
        >
          <ProvingTimeHistogram
            labels={histBuckets.labels}
            counts={histBuckets.counts}
            bucketS={histBuckets.bucketS}
            percentiles={[
              { label: 'P50', ms: summary.p50Ms },
              { label: 'P90', ms: summary.p90Ms },
              { label: 'P95', ms: summary.p95Ms },
              { label: 'P99', ms: summary.p99Ms },
            ]}
          />
        </ChartPanel>

        <ChartPanel
          title="Phase Breakdown"
          subtitle={breakdownSubtitle}
          action={
            overlap ? (
              <button
                type="button"
                onClick={() => setPhaseTimingMode(m => (m === 'perNode' ? 'clusterTotal' : 'perNode'))}
                className={cx(PILL, FOCUS_RING, PILL_IDLE)}
              >
                {phaseTimingMode === 'perNode' ? 'Mean' : 'Sum'}
              </button>
            ) : undefined
          }
        >
          <PhaseBreakdownChart rows={phaseRows} registry={registry} />
        </ChartPanel>

        <ChartPanel
          title="Phase breakdown per block"
          subtitle="Each phase ends when the last node finishes it."
          action={
            <button
              type="button"
              onClick={() => setSortByTime(v => !v)}
              className={cx(PILL, FOCUS_RING, PILL_IDLE)}
            >
              {sortByTime ? 'Sort by time' : 'Sort by name'}
            </button>
          }
        >
          <PhaseTimingChart labels={phaseChart.labels} values={phaseChart.values} registry={registry} total={phaseChart.total} />
        </ChartPanel>

        <ChartPanel title="Gas used vs proving time">
          <ScatterLine blocks={blocks} height={300} />
        </ChartPanel>
      </section>
    </div>
  );
}

// Benchmark start timestamp rendered as a date followed by the time, joined with a space.
const benchmarkTime = (ms: number): string =>
  formatDateTime(
    ms,
    { year: 'numeric', month: 'short', day: '2-digit' },
    { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false }
  );
