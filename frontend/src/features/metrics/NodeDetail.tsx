/*
 * Node detail panel, the right side of the Metrics page. Leads with the node's whole-run GPU summary,
 * then per-block phase timings, then per-GPU telemetry. The phase chart and telemetry share one zoom
 * window held in seconds, so scrolling either keeps both framed on the same span, and the window survives
 * a node switch because seconds are node-independent. The heading close control dismisses the panel.
 */

import { useCallback, useEffect, useMemo, useRef } from 'react';
import { DetailPanel } from '@/components/common/DetailPanel';
import { ChartPanel } from '@/components/common/ChartPanel';
import { SectionHeading } from '@/components/common/SectionHeading';
import { StatStrip, type StatItem } from '@/components/common/StatStrip';
import { PhaseTimingChart } from '@/components/charts/PhaseTimingChart';
import { GpuTelemetry } from '@/components/charts/GpuTelemetry';
import type { ChartInstance } from '@/components/charts/EChart';
import { useBenchDerived } from '@/hooks/useBenchDerived';
import { hasOverlapPhases, nodePhaseSeries } from '@/utils/phaseTimings';
import { createNodeWindowMap } from '@/utils/nodeWindow';
import { formatSeconds, formatMiBps, msToSec } from '@/utils/format';
import type { Block, NodeStats, Run } from '@/types/benchmark';

// GPU summary stats for the focused node's lead section, from the parser's whole-run per-node summary.
function gpuKpis(stats: NodeStats): StatItem[] {
  const { mean_sm, mean_mem, max_temp, peak_rxpci } = stats;
  // Each averaged field is omitted when the node has no telemetry so the strip never prints a null.
  return [
    ...(mean_sm == null ? [] : [{ label: 'GPU mean SM util', value: `${mean_sm.toFixed(0)}%` }]),
    ...(mean_mem == null ? [] : [{ label: 'GPU mean MEM util', value: `${mean_mem.toFixed(0)}%` }]),
    ...(max_temp == null ? [] : [{ label: 'GPU max temp', value: `${max_temp} C` }]),
    { label: 'Thermal throttle', value: formatSeconds(stats.temp_throttle_seconds, 1) },
    ...(peak_rxpci == null ? [] : [{ label: 'Peak PCIe RX', value: formatMiBps(peak_rxpci) }]),
  ];
}

export function NodeDetail({ run, node, onClose }: { run: Run; node: string; onClose: () => void }) {
  const { nodes, registry, telemetryById } = useBenchDerived(run);
  // Focused node id resolves to its hardware-list index, the key into every positional array.
  const nodeIndex = nodes.indexOf(node);
  const stats = nodeIndex >= 0 ? run.statistics.nodes[nodeIndex] : undefined;

  const phase = useMemo(() => nodePhaseSeries(run.blocks, nodeIndex, registry), [run.blocks, nodeIndex, registry]);
  // An overlap preset's phases run concurrently, so this chart draws the total alone rather than a line
  // per phase, and then has no per-phase rule to explain.
  const overlap = hasOverlapPhases(registry);

  // A block's wall-clock span in ms. A successful proof reports it directly, a crashed one does not so
  // its span runs to the latest of this node's partial phase ends and its crash moment, keeping the
  // telemetry window aligned to a block even when the proof never finished.
  const blockSpanMs = useCallback(
    (b: Block): number => {
      if (b.proving_ms != null) return b.proving_ms;
      const bnode = b.nodes[nodeIndex];
      if (!bnode) return 0;
      const phaseEnd = bnode.phases.reduce((m, w) => (w ? Math.max(m, w.start_ms + w.dur_ms) : m), 0);
      return Math.max(phaseEnd, bnode.crashed_ms ?? 0);
    },
    [nodeIndex]
  );

  // Telemetry sits on the run epoch so origin is zero and the axis reads seconds since run start. The
  // full run spans zero to the last block's end, the extent the shared window narrows.
  const origin = 0;
  const runEndSec = useMemo(
    () => phase.blocks.reduce((m, p) => Math.max(m, msToSec(p.start_ms + blockSpanMs(p))), 0),
    [phase.blocks, blockSpanMs]
  );
  // Raw per-block proving windows in seconds, in block-array order. A crashed proof arrives as a zero
  // pair, which the window map folds into a monotonic anchor so the seconds bridge is never inverted.
  const blockWindows = useMemo(
    () => phase.blocks.map(p => [msToSec(p.start_ms), msToSec(p.start_ms + blockSpanMs(p))] as [number, number]),
    [phase.blocks, blockSpanMs]
  );

  // Single source of truth is the shared seconds window, held in a ref so a wheel never re-renders the
  // panel or rebuilds the charts. The map converts it to each axis (phase by covered block indices,
  // telemetry by real seconds) so the two frame the same span, and a node switch keeps the seconds
  // window because every node shares one run clock.
  const map = useMemo(() => createNodeWindowMap(blockWindows, runEndSec), [blockWindows, runEndSec]);
  // Smallest zoom window in seconds, a three-block span so a deep zoom holds at a legible block count and
  // the telemetry floor matches the phase chart's.
  const minSpanSec = useMemo(
    () => (map.last > 0 ? (Math.min(map.last, 2) / map.last) * runEndSec : runEndSec),
    [map.last, runEndSec]
  );

  const viewRef = useRef<[number, number]>([0, runEndSec]);
  const phaseChart = useRef<ChartInstance | null>(null);
  const telChart = useRef<ChartInstance | null>(null);
  // Which chart's next datazoom event is a programmatic echo to swallow, so a synced dispatch never loops
  // back through the relay and drifts the window.
  const echo = useRef<'phase' | 'tel' | null>(null);

  // Phase and telemetry getters read the shared seconds window on rebuild so a legend toggle keeps the
  // window without making the option depend on it, which would rebuild per wheel tick.
  const getPhaseZoom = useCallback((): [number, number] => map.phasePct(viewRef.current), [map]);
  const getTelZoom = useCallback((): [number, number] => map.telPct(viewRef.current), [map]);

  // Drive one chart to a seconds window through its own axis, marking the dispatch as an echo so the
  // event it fires is swallowed not relayed back.
  const drive = useCallback(
    (which: 'phase' | 'tel', win: [number, number]): void => {
      const inst = which === 'phase' ? phaseChart.current : telChart.current;
      if (!inst) return;
      const [s, e] = which === 'phase' ? map.phasePct(win) : map.telPct(win);
      echo.current = which;
      inst.dispatchAction({ type: 'dataZoom', start: s, end: e });
      echo.current = null;
    },
    [map]
  );

  // A wheel or drag on either chart resolves to a seconds window. The echo of a synced dispatch is
  // swallowed first. A zoom narrower than the floor is held so the window neither shrinks past three
  // blocks nor glides, only zooming out is allowed. The other chart is driven to the same span, and the
  // source is snapped back when block rounding or the floor moved its window off the target.
  const relayZoom = useCallback(
    (sPct: number, ePct: number, source: 'phase' | 'tel'): void => {
      if (echo.current === source) {
        echo.current = null;
        return;
      }
      const win = source === 'phase' ? map.fromPhasePct(sPct, ePct) : map.fromTelPct(sPct, ePct);
      const prev = viewRef.current;
      // Forbid only a window narrower than the floor, a pan at the floor keeps its span and is allowed.
      const target: [number, number] = win[1] - win[0] < minSpanSec - 1e-6 ? prev : win;
      const changed = target[0] !== prev[0] || target[1] !== prev[1];
      viewRef.current = target;

      if (changed) drive(source === 'phase' ? 'tel' : 'phase', target);
      // Snap the source back when it lands off the target (held window or block rounding) so it holds.
      const [ss, se] = source === 'phase' ? map.phasePct(target) : map.telPct(target);
      if (Math.abs(ss - sPct) > 0.01 || Math.abs(se - ePct) > 0.01) drive(source, target);
    },
    [map, minSpanSec, drive]
  );

  const onPhaseZoom = useCallback((s: number, e: number) => relayZoom(s, e, 'phase'), [relayZoom]);
  const onTelZoom = useCallback((s: number, e: number) => relayZoom(s, e, 'tel'), [relayZoom]);

  // The hovered block paints its proving window on the telemetry. The paint is imperative through the
  // telemetry handle and deferred to the next frame, so a hover never re-renders React or stalls the
  // phase tooltip, and rapid block crossings coalesce to one paint.
  const highlightApi = useRef<((window: [number, number] | null) => void) | null>(null);
  const registerHighlight = useCallback((paint: ((window: [number, number] | null) => void) | null) => {
    highlightApi.current = paint;
  }, []);
  const hoverFrame = useRef(0);
  const onHoverBlock = useCallback(
    (index: number | null) => {
      cancelAnimationFrame(hoverFrame.current);
      hoverFrame.current = requestAnimationFrame(() => {
        highlightApi.current?.(index != null ? map.blockBand(index) : null);
      });
    },
    [map]
  );
  useEffect(() => () => cancelAnimationFrame(hoverFrame.current), []);

  // A node switch clears the hover band and keeps the shared seconds window, clamped to the new run
  // length. The option rebuild reapplies it through the getters above, so the new node opens framed on
  // the same span the previous one showed rather than drifting or resetting.
  useEffect(() => {
    highlightApi.current?.(null);
    const w = viewRef.current;
    viewRef.current = [Math.max(0, Math.min(runEndSec, w[0])), Math.max(0, Math.min(runEndSec, w[1]))];
  }, [node, runEndSec]);

  return (
    <DetailPanel onClose={onClose} closeLabel="Close node detail">
      {stats && <StatStrip items={gpuKpis(stats)} />}

      {/* Same titled-panel presentation as the Overview "Phase breakdown per block", without the sort
          toggle, so the two read as one design rather than two near-identical styles. */}
      <ChartPanel
        title="Phase breakdown per block"
        subtitle={
          overlap
            ? undefined
            : "Each phase is this node's own time. Total is the whole proof, which runs longer while the node idles waiting for aggregation."
        }
      >
        <PhaseTimingChart
          labels={phase.labels}
          values={phase.values}
          registry={registry}
          total={phase.total}
          totalOnly={overlap}
          getZoom={getPhaseZoom}
          onZoom={onPhaseZoom}
          onHoverBlock={onHoverBlock}
          minValueSpan={2}
          onReady={inst => (phaseChart.current = inst)}
        />
      </ChartPanel>

      {/* GPU telemetry is a group of per-metric panels under one heading and a shared legend. */}
      <section className="flex flex-col gap-4">
        <SectionHeading>GPU telemetry</SectionHeading>
        <GpuTelemetry
          telemetry={telemetryById}
          metricDefs={run.telemetry.metrics}
          nodes={[node]}
          origin={origin}
          windowSec={[0, runEndSec]}
          getZoom={getTelZoom}
          onZoom={onTelZoom}
          minValueSpan={minSpanSec}
          onReady={inst => (telChart.current = inst)}
          registerHighlight={registerHighlight}
          group={`gpu-${node}`}
        />
      </section>
    </DetailPanel>
  );
}
