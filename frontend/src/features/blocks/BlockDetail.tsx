/*
 * Block detail panel, the right side of the merged Blocks page. Renders one block's overview figures,
 * per-node timeline, and GPU telemetry over the proving window. The shared DetailPanel supplies the
 * scrolling shell, the Overview heading with a status badge, and the close control.
 */

import { useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import { DetailPanel } from '@/components/common/DetailPanel';
import { ChartPanel } from '@/components/common/ChartPanel';
import { SectionHeading } from '@/components/common/SectionHeading';
import { StatStrip } from '@/components/common/StatStrip';
import { GpuTelemetry } from '@/components/charts/GpuTelemetry';
import { IconButton } from '@/components/common/IconButton';
import { IconFullscreen, IconPipeline } from '@/components/common/icons';
import type { PhaseBand } from '@/components/charts/MetricChart';
import { cx } from '@/utils/cx';
import { useBench } from '@/hooks/useBench';
import { useBenchDerived } from '@/hooks/useBenchDerived';
import type { PhaseRegistry } from '@/utils/phases';
import { windowSeconds, hasTimeline } from '@/utils/phaseTimings';
import { msToSec } from '@/utils/format';
import { blockOverviewFields } from '@/features/blocks/blockOverview';
import { BlockTrace } from '@/features/blocks/BlockTrace';
import { BlockTraceFullscreen } from '@/features/blocks/BlockTraceFullscreen';
import { BlockPipelineFullscreen } from '@/features/blocks/BlockPipelineFullscreen';
import { hasPipeline } from '@/utils/pipelineItems';
import { statusColor } from '@/features/blocks/blockFilters';
import type { Block, Run } from '@/types/benchmark';

// Per-node proving-phase windows in seconds since block start, banding the telemetry when a single node
// is in focus. Each window comes from that node's own phase windows, so the aggregate phase appears only
// on the aggregator's row.
function phaseBands(block: Block, nodes: string[], registry: PhaseRegistry): Record<string, PhaseBand[]> {
  const out: Record<string, PhaseBand[]> = {};
  block.nodes.forEach((node, i) => {
    const id = nodes[i];
    if (!id) return;
    const bands: PhaseBand[] = registry.list.flatMap(phase => {
      const sec = windowSeconds(node.phases[phase.index] ?? null);
      return sec ? [{ label: phase.label, color: phase.color, start: sec.start, end: sec.end }] : [];
    });
    out[id] = bands.filter(b => b.end > b.start);
  });
  return out;
}

export function BlockDetail({ run, block, onClose }: { run: Run; block: Block; onClose: () => void }) {
  const bench = useBench();
  const { nodes, registry, telemetryById } = useBenchDerived(run);
  const [fullscreen, setFullscreen] = useState(false);
  const [pipelineFullscreen, setPipelineFullscreen] = useState(false);

  const provingSec = block.proving_ms != null ? msToSec(block.proving_ms) : null;
  // End of the trace in seconds. A successful proof ends at its proving time, a crashed proof has none
  // so the trace runs to the latest of its partial phase ends and any node's crash moment.
  const timelineEndSec = useMemo(() => {
    const ends = block.nodes.flatMap(node => {
      const phaseEnds = registry.list.map(p => windowSeconds(node.phases[p.index] ?? null)?.end ?? 0);
      return [...phaseEnds, node.crashed_ms != null ? msToSec(node.crashed_ms) : 0];
    });
    const maxEnd = ends.length ? Math.max(0, ...ends) : 0;
    return provingSec ?? (maxEnd > 0 ? maxEnd : 1);
  }, [block, registry, provingSec]);
  // Telemetry origin is the block start as an offset from the run epoch, so the x-axis reads as seconds
  // since proving start, and the window spans the trace with a one-second pad on each side.
  const origin = block.start_ms;
  const windowSec = useMemo<[number, number]>(() => [-1, timelineEndSec + 1], [timelineEndSec]);
  const bandsByNode = useMemo(() => phaseBands(block, nodes, registry), [block, nodes, registry]);

  return (
    <DetailPanel
      onClose={onClose}
      closeLabel="Close block detail"
      aside={
        block.status !== 'success' ? (
          <span className={cx('text-sm font-semibold', statusColor(block.status))}>{block.status}</span>
        ) : null
      }
    >
      <StatStrip items={blockOverviewFields().map(f => ({ label: f.label, value: f.render(block, registry) }))} />

      <ChartPanel
        title="Trace"
        action={
          hasTimeline(block) || hasPipeline(bench, block) ? (
            <div className="flex items-center gap-1">
              {hasTimeline(block) && (
                <IconButton onClick={() => setFullscreen(true)} label="Open trace fullscreen">
                  <IconFullscreen />
                </IconButton>
              )}
              {hasPipeline(bench, block) && (
                <IconButton onClick={() => setPipelineFullscreen(true)} label="Open pipeline fullscreen">
                  <IconPipeline />
                </IconButton>
              )}
            </div>
          ) : undefined
        }
      >
        <BlockTrace block={block} nodes={nodes} registry={registry} />
      </ChartPanel>

      {/* GPU telemetry is a group of per-metric panels, each a titled chart like the Trace, under one
          group heading and a shared legend rather than a single panel. */}
      <section className="flex flex-col gap-4">
        <div>
          <SectionHeading>GPU telemetry</SectionHeading>
          <p className="mt-1 text-xs text-faint">
            Seconds since proving start. The shaded band marks the proof window. Select a single node to overlay its
            phases.
          </p>
        </div>
        <GpuTelemetry
          telemetry={telemetryById}
          metricDefs={run.telemetry.metrics}
          nodes={nodes}
          origin={origin}
          windowSec={windowSec}
          phaseBandsByNode={bandsByNode}
          highlight={provingSec != null ? [0, provingSec] : undefined}
          zoomable={false}
          group="proof-window"
        />
      </section>

      {fullscreen &&
        createPortal(
          <BlockTraceFullscreen
            benchId={bench.id}
            runId={run.id}
            block={block}
            nodes={nodes}
            registry={registry}
            onClose={() => setFullscreen(false)}
          />,
          document.body
        )}

      {pipelineFullscreen &&
        createPortal(
          <BlockPipelineFullscreen
            bench={bench}
            block={block}
            nodes={nodes}
            registry={registry}
            onClose={() => setPipelineFullscreen(false)}
          />,
          document.body
        )}
    </DetailPanel>
  );
}
