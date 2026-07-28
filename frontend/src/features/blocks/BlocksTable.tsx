/*
 * Merged blocks table, one row per block per run, defaulting to each block's latest attempt so a patched
 * benchmark reads as one result. A name links to the detail panel. The filter state and the shown set
 * are owned by the page through useBlockTableModel, so the filter row can sit above the split. This
 * component renders only the table from that model.
 */

import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import { DataTable, type DataColumn } from '@/components/common/DataTable';
import { runColumn } from '@/components/common/gpuColumns';
import { ProportionBar } from '@/components/common/ProportionBar';
import { Truncated } from '@/components/common/Truncated';
import { cx } from '@/utils/cx';
import { FOCUS_RING } from '@/utils/styles';
import { blockLabel, type PhaseRegistry } from '@/utils/phases';
import { hasOverlapPhases } from '@/utils/phaseTimings';
import { phaseMixSegments, blockOverviewFields } from '@/features/blocks/blockOverview';
import { statusColor, type BlockRow } from '@/features/blocks/blockFilters';
import type { BlockTableModel } from '@/features/blocks/useBlockTableModel';
import type { Benchmark, Block } from '@/types/benchmark';

// Proving time, paired with a phase-mix bar where the phases run in sequence. Fixed-width track and
// column align bars and figures down the column. The bar scales against the slowest proof. A preset
// whose phases overlap has no sequence to split, so its rows carry the figure alone.
function ProvingTimeCell({
  block,
  registry,
  maxProvingMs,
  text,
  phaseMix,
}: {
  block: Block;
  registry: PhaseRegistry;
  maxProvingMs: number;
  text: string;
  phaseMix: boolean;
}) {
  const widthPct = maxProvingMs > 0 && block.proving_ms != null ? (block.proving_ms / maxProvingMs) * 100 : 100;
  return (
    <div className="flex items-center gap-3">
      <span className="w-16 shrink-0 text-right tabular-nums">{text}</span>
      {phaseMix && (
        <div className="w-28 shrink-0">
          {block.status === 'success' && (
            <div style={{ width: `${widthPct}%` }}>
              <ProportionBar segments={phaseMixSegments(block, registry)} />
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// Composite row key, unique because one block name can recur across runs. Matches the page's active key
// so the open block's row highlights.
const rowKeyOf = (r: BlockRow): string => `${r.runIndex}/${r.block.name}`;

export function BlocksTable({
  model,
  bench,
  search,
  activeKey,
  onVisibleRowsChange,
}: {
  model: BlockTableModel;
  bench: Benchmark;
  search: string;
  activeKey?: string;
  // The rows in displayed order, reported up so the page steps arrow-key navigation through the
  // filtered and sorted set the reader sees.
  onVisibleRowsChange?: (rows: BlockRow[]) => void;
}) {
  const { registry, rows, maxProvingMs } = model;

  // Columns lead with id, proving time, status, then the overview fields, run index, and latest flag.
  // Proving time keeps the shared sort value but renders the phase-mix bar, which an overlap preset's
  // concurrent phases cannot be split into.
  const columns = useMemo<DataColumn<BlockRow>[]>(() => {
    const fields = blockOverviewFields(bench);
    const phaseMix = !hasOverlapPhases(registry);
    const time = fields.find(f => f.key === 'time');
    const rest = fields.filter(f => f.key !== 'time');
    return [
      {
        key: 'id',
        // The id has no natural width bound, so it starts compact and the reader widens it to see a long
        // id in full.
        width: 352,
        header: 'ID',
        render: r => (
          <Link
            to={{ pathname: `/blocks/${r.runIndex}/${encodeURIComponent(r.block.name)}`, search }}
            className={cx('flex w-full min-w-0 rounded-sm font-medium text-primary hover:underline', FOCUS_RING)}
          >
            <Truncated text={blockLabel(r.block)} />
          </Link>
        ),
        // Ids sort lexically, keeping a fixture's variants together and ordering zero-padded block ids
        // numerically.
        sortValue: r => r.block.name,
      },
      ...(time
        ? [
            {
              key: 'time',
              header: time.label,
              render: (r: BlockRow) => (
                <ProvingTimeCell
                  block={r.block}
                  registry={registry}
                  maxProvingMs={maxProvingMs}
                  text={time.render(r.block, registry)}
                  phaseMix={phaseMix}
                />
              ),
              sortValue: (r: BlockRow) => time.sortValue(r.block, registry),
            },
          ]
        : []),
      {
        key: 'status',
        header: 'Status',
        render: r => <span className={cx('font-medium', statusColor(r.block.status))}>{r.block.status}</span>,
      },
      ...rest.map(f => ({
        key: f.key,
        header: f.label,
        render: (r: BlockRow) => f.render(r.block, registry),
        sortValue: (r: BlockRow) => f.sortValue(r.block, registry),
      })),
      // Run index identifies the source run, the run id reading on hover.
      runColumn<BlockRow>(),
      {
        key: 'is_latest',
        header: 'Latest',
        render: r => <span className={r.isLatest ? 'text-success' : 'text-muted'}>{r.isLatest ? 'Yes' : 'No'}</span>,
        sortValue: r => (r.isLatest ? 1 : 0),
      },
    ];
  }, [bench, search, registry, maxProvingMs]);

  return (
    <div className="flex h-full min-h-0 flex-col">
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={rowKeyOf}
        activeRowKey={activeKey}
        onVisibleRowsChange={onVisibleRowsChange}
        initialSort={{ key: 'id', dir: 'asc' }}
        // Scope persisted widths per benchmark so a long-id benchmark keeps its wide id column.
        tableId={`blocks:${bench.id}`}
        className="min-h-0 flex-1 overflow-y-auto"
      />
    </div>
  );
}
