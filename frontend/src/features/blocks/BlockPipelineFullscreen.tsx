/*
 * Fullscreen overlay of a block's fine pipeline, the node legend, phase legend, and time strip above
 * the virtualized waterfall timeline. Items decode once per block, and the node legend filters them
 * ahead of the window slice Grafana-style, where a plain click isolates one node, a plain click on the
 * lone selected node resets to all, and a modifier click toggles one. The strip's slider owns the zoom
 * window the timeline maps its x extent onto, and hovering the timeline shows a time readout chip over
 * the strip's axis lane. The frame stays up when keyboard navigation lands on a block that decodes to
 * no items, showing the empty note in place of the timeline. The overlay floats over the page and
 * dismisses on the close control, a press on the backdrop, or Escape.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Modal } from '@/components/common/Modal';
import { ColorDot } from '@/components/common/ColorDot';
import { EmptyState } from '@/components/common/EmptyState';
import { GroupedLegend, type LegendGroup } from '@/components/common/GroupedLegend';
import { PipelineTimeline, PipelineTimeStrip } from '@/components/charts/PipelineTimeline';
import { decodePipeline } from '@/utils/pipelineItems';
import { grafanaSelect } from '@/utils/chartHelpers';
import { nodeColorById } from '@/utils/dataVizColors';
import { formatSeconds } from '@/utils/format';
import type { PhaseRegistry } from '@/utils/phases';
import type { Benchmark, Block } from '@/types/benchmark';

export function BlockPipelineFullscreen({
  bench,
  block,
  nodes,
  registry,
  onClose,
}: {
  bench: Benchmark;
  block: Block;
  nodes: string[];
  registry: PhaseRegistry;
  onClose: () => void;
}) {
  const model = useMemo(() => decodePipeline(bench, block, nodes, registry), [bench, block, nodes, registry]);
  // The CPU/GPU opacity note applies only when some item marks its heavy sides.
  const hasHeavyMarkers = useMemo(
    () => model.items.some(item => item.cpuHeavy != null || item.gpuHeavy != null),
    [model]
  );
  const [selected, setSelected] = useState<Set<string>>(() => new Set(nodes));
  const shown = useMemo(
    () => (selected.size === nodes.length ? model.items : model.items.filter(item => selected.has(item.nodeId))),
    [model, selected, nodes.length]
  );
  const hasItems = model.items.length > 0;
  // The time-axis zoom window in percent, owned here so the strip's slider and the timeline's x
  // extent read one state.
  const [zoom, setZoom] = useState<[number, number]>([0, 100]);
  const scrollRef = useRef<HTMLDivElement | null>(null);

  // The hovered-time chip over the strip's axis lane, written imperatively per hover report so a
  // mousemove never re-renders the overlay or touches the band's option. Its canvas x transfers
  // directly since the strip and the band share the same width and horizontal insets, its opaque
  // background covers the axis labels beneath it instead of colliding with them, and its center
  // clamps inside the strip so it never spills past either edge.
  const hoverChipRef = useRef<HTMLDivElement | null>(null);
  const onHover = useCallback((hover: { sec: number; x: number } | null): void => {
    const chip = hoverChipRef.current;
    if (!chip) return;
    if (!hover) {
      chip.style.visibility = 'hidden';
      return;
    }
    chip.style.visibility = 'visible';
    chip.textContent = formatSeconds(hover.sec, 3);
    const half = chip.offsetWidth / 2;
    chip.style.left = `${Math.min(Math.max(hover.x, half), chip.parentElement!.clientWidth - half)}px`;
  }, []);

  // Keyboard navigation under a stationary cursor would leave the chip showing the prior block's
  // time, so a model change hides it.
  useEffect(() => onHover(null), [model, onHover]);

  const legendGroups: LegendGroup[] = useMemo(
    () => [
      {
        // Bare node digit so the group reads "Node 1 2 3 4" without a redundant "node" prefix.
        name: 'Node',
        items: nodes.map(n => ({ key: n, label: n.replace(/^node/, '') || n, color: nodeColorById(n) })),
        selected,
        onToggle: (key, multi) => setSelected(prev => grafanaSelect(prev, nodes, key, multi)),
      },
    ],
    [nodes, selected]
  );

  return (
    <Modal
      title="Pipeline"
      ariaLabel="Block pipeline"
      onDismiss={onClose}
      closeLabel="Close pipeline fullscreen"
      panelClassName="fixed inset-y-6 left-1/2 z-50 flex w-[calc(100%-3rem)] max-w-[1280px] -translate-x-1/2 flex-col gap-3 rounded-xl border border-border bg-elevated p-4 shadow-2xl"
    >
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 px-1 text-xs text-muted">
        <GroupedLegend groups={legendGroups} orientation="horizontal" />
        {model.phasesUsed.map(name => (
          <ColorDot key={name} color={registry.color(name)} label={registry.label(name)} />
        ))}
        {hasHeavyMarkers && (
          <span className="text-faint">Lighter segment is the CPU heavy workload, solid is the GPU one.</span>
        )}
      </div>
      {hasItems ? (
        <>
          {/* Both wrappers reserve a stable scrollbar gutter so the strip and the band keep the
              same plot width and their ticks stay aligned when the timeline overflows. */}
          <div className="overflow-y-auto [scrollbar-gutter:stable]">
            <div className="relative">
              <PipelineTimeStrip endSec={model.endSec} zoom={zoom} onZoom={setZoom} />
              <div
                ref={hoverChipRef}
                className="pointer-events-none absolute top-0.5 -translate-x-1/2 rounded border border-border bg-elevated px-1 text-[10px] tabular-nums text-foreground"
                style={{ visibility: 'hidden' }}
              />
            </div>
          </div>
          <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto [scrollbar-gutter:stable]">
            <PipelineTimeline
              items={shown}
              endSec={model.endSec}
              zoom={zoom}
              onHover={onHover}
              registry={registry}
              scrollRef={scrollRef}
            />
          </div>
        </>
      ) : (
        <EmptyState tone="faint" as="div" className="flex min-h-0 flex-1 items-center justify-center">
          No pipeline events for this block.
        </EmptyState>
      )}
    </Modal>
  );
}
