/*
 * Blocks page, a filter row above a master-detail split of the blocks table and a block detail panel.
 * The filter row sits above the split, full width, so opening a block never reflows it onto a second
 * line. The open block lives at /blocks/{run_index}/{block_name} since a name can recur across runs.
 * Wiring is useMasterDetailRoute, and the filter and table share one model from useBlockTableModel.
 */

import { useCallback, useMemo, useState } from 'react';
import { useParams } from 'react-router-dom';
import { useBench } from '@/hooks/useBench';
import { useRunSearch } from '@/hooks/useRunSearch';
import { useMasterDetailRoute } from '@/hooks/useMasterDetailRoute';
import { useListArrowNav } from '@/hooks/useListArrowNav';
import { runByIndex } from '@/utils/runs';
import { ResizableSplit } from '@/components/layout/ResizableSplit';
import { BlocksFilters } from '@/features/blocks/BlocksFilters';
import { ReRunCommand } from '@/features/blocks/ReRunCommand';
import { BlocksTable } from '@/features/blocks/BlocksTable';
import { BlockDetail } from '@/features/blocks/BlockDetail';
import { useBlockTableModel } from '@/features/blocks/useBlockTableModel';
import type { BlockRow } from '@/features/blocks/blockFilters';

export function BlocksPage() {
  const bench = useBench();
  const { runIdx, blockId } = useParams();
  const search = useRunSearch();
  const model = useBlockTableModel(bench);

  // Every block in the open block's run, the set the open block resolves against so it stays open even
  // when the filter hides it. The arrow keys instead step the displayed rows, captured from the table.
  const all = useMemo(() => {
    const run = runByIndex(bench, runIdx);
    return run ? run.blocks : [];
  }, [bench, runIdx]);

  const { run, selected, activeKey, onClose } = useMasterDetailRoute({
    itemParam: blockId,
    items: all,
    idOf: b => b.name,
    basePath: '/blocks',
  });

  // The rows the table shows, filtered and sorted, the set the arrow keys step through. It lands after
  // the table's first render, so it falls back to the filtered set until the sorted order arrives. Each
  // row carries its own run, so stepping across runs navigates each to its own attempt, matching the
  // displayed Run column rather than forcing them onto the open block's run.
  const [shown, setShown] = useState<BlockRow[]>([]);
  const onVisibleRowsChange = useCallback((rows: BlockRow[]) => setShown(rows), []);
  const navRows = shown.length ? shown : model.rows;

  useListArrowNav<BlockRow>({
    enabled: selected != null,
    currentKey: activeKey,
    items: navRows,
    keyOf: r => `${r.runIndex}/${r.block.name}`,
    pathOf: r => `/blocks/${r.runIndex}/${encodeURIComponent(r.block.name)}`,
  });

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <BlocksFilters
          filters={model.filters}
          onChange={model.setFilters}
          bounds={model.bounds}
          shown={model.rows.length}
          total={model.allRows.length}
        />
        {model.reRunBlocks.length > 0 && (
          <ReRunCommand
            benchId={bench.id}
            guest={bench.software.guest.name}
            blocks={model.reRunBlocks}
            allSelected={model.allSelected}
          />
        )}
      </div>
      <div className="min-h-0 flex-1">
        <ResizableSplit
          storageKey="blocks-panel-fraction"
          resizeLabel="Resize block detail"
          left={
            <BlocksTable
              model={model}
              bench={bench}
              search={search}
              activeKey={activeKey}
              onVisibleRowsChange={onVisibleRowsChange}
            />
          }
          right={run && selected ? <BlockDetail run={run} block={selected} onClose={onClose} /> : null}
        />
      </div>
    </div>
  );
}
