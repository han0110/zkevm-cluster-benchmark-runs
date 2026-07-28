/*
 * Benchmark selector as a modal table listing every benchmark with its time, software, description, and
 * id, ordered newest first. Each row's metadata is read on demand through loadBenchmarkMeta, so opening
 * the picker never loads the full documents. The modal dismisses on the close control, a backdrop press,
 * or Escape.
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import { cx } from '@/utils/cx';
import { FOCUS_RING, OVERLINE, ROW_BASE, ROW_ACTIVE, ROW_IDLE } from '@/utils/styles';
import { IconChevronDown } from '@/components/common/icons';
import { Modal } from '@/components/common/Modal';
import { loadBenchmarkMeta, type BenchmarkMeta } from '@/utils/benchmarkMeta';
import { formatDateTime } from '@/utils/format';
import type { RunIndexEntry } from '@/types/benchmark';

interface BenchmarkPickerProps {
  entries: RunIndexEntry[];
  selectedId: string | null;
  // The selected benchmark's display name once its document has loaded, shown on the trigger in place of
  // the id. Null until the active document resolves, when the id stands in.
  selectedName: string | null;
  onSelect: (id: string) => void;
}

// The load state of one row's metadata, the id and url known from the index before the head resolves.
type MetaState =
  | { status: 'loading' }
  | { status: 'ready'; meta: BenchmarkMeta }
  | { status: 'error' };

// Benchmark time as a short local date and time, the run start the document carries.
const benchmarkAt = (ms: number | null): string =>
  ms == null
    ? '-'
    : formatDateTime(ms, { year: 'numeric', month: 'short', day: 'numeric' }, { hour: '2-digit', minute: '2-digit' });

// A software part as name@version, the merged cell the table shows and orders on.
const softwareLabel = (part: { name: string; version: string }): string => `${part.name}@${part.version}`;

// The columns the table orders by. The rest carry no natural order a reader would want.
type SortKey = 'startedAt' | 'guest' | 'zkvm';
type SortDir = 'asc' | 'desc';

// The value a row sorts on for the active column, null while its metadata head is still loading.
function sortValue(meta: BenchmarkMeta | null, key: SortKey): number | string | null {
  if (!meta) return null;
  if (key === 'startedAt') return meta.startedAt;
  return softwareLabel(key === 'guest' ? meta.software.guest : meta.software.zkvm);
}

// Loads every entry's metadata head once the picker is open, keyed by id. loadBenchmarkMeta caches per
// url, so reopening resolves from cache without another request.
function useBenchmarkMetas(entries: RunIndexEntry[], enabled: boolean): Record<string, MetaState> {
  const [metas, setMetas] = useState<Record<string, MetaState>>({});
  useEffect(() => {
    if (!enabled) return;
    let cancelled = false;
    entries.forEach(entry => {
      setMetas(prev => {
        const existing = prev[entry.id];
        const keepPrior = existing != null && existing.status !== 'error';
        return keepPrior ? prev : { ...prev, [entry.id]: { status: 'loading' } };
      });
      loadBenchmarkMeta(entry.url)
        .then(meta => {
          if (!cancelled) setMetas(prev => ({ ...prev, [entry.id]: { status: 'ready', meta } }));
        })
        .catch(() => {
          if (!cancelled) setMetas(prev => ({ ...prev, [entry.id]: { status: 'error' } }));
        });
    });
    return () => {
      cancelled = true;
    };
  }, [entries, enabled]);
  return metas;
}

export function BenchmarkPicker({ entries, selectedId, selectedName, onSelect }: BenchmarkPickerProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  const metas = useBenchmarkMetas(entries, open);
  // Newest first by default, the order a reader wants when the latest run is the one under review.
  const [sort, setSort] = useState<{ key: SortKey; dir: SortDir }>({ key: 'startedAt', dir: 'desc' });

  // A row whose head has not resolved yet has no value to order on, so it sinks to the bottom in either
  // direction and settles into place once its metadata arrives.
  const sorted = useMemo(() => {
    const dir = sort.dir === 'asc' ? 1 : -1;
    const valueOf = (id: string): number | string | null => {
      const state = metas[id];
      return sortValue(state?.status === 'ready' ? state.meta : null, sort.key);
    };
    return [...entries].sort((a, b) => {
      const av = valueOf(a.id);
      const bv = valueOf(b.id);
      if (av == null && bv == null) return 0;
      if (av == null) return 1;
      if (bv == null) return -1;
      if (typeof av === 'number' && typeof bv === 'number') return (av - bv) * dir;
      return String(av).localeCompare(String(bv)) * dir;
    });
  }, [entries, metas, sort]);

  // A new column opens ascending and the active one flips, matching the DataTable headers elsewhere.
  const toggleSort = (key: SortKey): void =>
    setSort(prev => (prev.key === key ? { key, dir: prev.dir === 'asc' ? 'desc' : 'asc' } : { key, dir: 'asc' }));

  const choose = (id: string): void => {
    onSelect(id);
    setOpen(false);
  };

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen(o => !o)}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label="Select benchmark"
        className={cx(
          'flex w-full items-center justify-between gap-2 rounded-lg border border-border bg-surface px-3 py-2 text-sm font-medium text-foreground transition-colors hover:border-primary/60',
          FOCUS_RING
        )}
      >
        <span className="truncate">{selectedName ?? selectedId ?? 'Select benchmark'}</span>
        <IconChevronDown className="shrink-0 text-faint" />
      </button>

      {open && (
        <Modal
          title="Select benchmark"
          ariaLabel="Select benchmark"
          onDismiss={() => setOpen(false)}
          closeLabel="Close benchmark picker"
          containerClassName="fixed inset-0 z-50 grid place-items-center p-6"
          panelClassName="flex max-h-[80vh] w-full max-w-[1100px] flex-col gap-3 overflow-hidden rounded-xl border border-border bg-elevated p-4 shadow-2xl"
          extraRefs={[triggerRef]}
        >
          <div className="min-h-0 flex-1 overflow-auto rounded-lg border border-border bg-surface">
            <table className="w-full text-sm">
              <thead className="sticky top-0 z-10 bg-surface">
                <tr className={cx('border-b border-border text-left', OVERLINE)}>
                  <SortHeader label="Benchmark at" sortKey="startedAt" sort={sort} onSort={toggleSort} />
                  <SortHeader label="Guest" sortKey="guest" sort={sort} onSort={toggleSort} />
                  <SortHeader label="zkVM" sortKey="zkvm" sort={sort} onSort={toggleSort} />
                  <th className="whitespace-nowrap px-4 py-2.5 font-semibold">Description</th>
                  <th className="whitespace-nowrap px-4 py-2.5 font-semibold">ID</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-border">
                {sorted.map(entry => {
                  const state = metas[entry.id];
                  const meta = state?.status === 'ready' ? state.meta : null;
                  const failed = state?.status === 'error';
                  const active = entry.id === selectedId;
                  return (
                    <tr
                      key={entry.id}
                      onClick={() => choose(entry.id)}
                      aria-selected={active}
                      className={cx('cursor-pointer', ROW_BASE, active ? ROW_ACTIVE : ROW_IDLE)}
                    >
                      <td className="whitespace-nowrap px-4 py-2.5 font-medium tabular-nums">
                        {meta ? benchmarkAt(meta.startedAt) : <PickerCell value={undefined} failed={failed} />}
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5">
                        <PickerCell value={meta && softwareLabel(meta.software.guest)} failed={failed} />
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5">
                        <PickerCell value={meta && softwareLabel(meta.software.zkvm)} failed={failed} />
                      </td>
                      <td className="max-w-md truncate px-4 py-2.5 text-muted" title={meta?.description}>
                        <PickerCell value={meta?.description} failed={failed} muted />
                      </td>
                      <td className="whitespace-nowrap px-4 py-2.5 font-mono text-xs text-faint">{entry.id}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {entries.length === 0 && <p className="p-4 text-sm text-muted">No benchmarks found.</p>}
          </div>
        </Modal>
      )}
    </>
  );
}

// One sortable column header, its label a button that toggles the order. The indicator is always present
// and only its glyph toggles, so the header keeps its width when sorted rather than shifting the layout.
// The markup matches the DataTable headers so the two tables read as one design.
function SortHeader({
  label,
  sortKey,
  sort,
  onSort,
}: {
  label: string;
  sortKey: SortKey;
  sort: { key: SortKey; dir: SortDir };
  onSort: (key: SortKey) => void;
}) {
  const dir = sort.key === sortKey ? sort.dir : null;
  return (
    <th
      aria-sort={dir ? (dir === 'asc' ? 'ascending' : 'descending') : undefined}
      className="whitespace-nowrap px-4 py-2.5 font-semibold"
    >
      <button
        type="button"
        onClick={() => onSort(sortKey)}
        className={cx(
          'inline-flex max-w-full items-center gap-1 truncate rounded-sm hover:text-foreground',
          FOCUS_RING,
          dir && 'text-foreground'
        )}
      >
        {label}
        <span aria-hidden="true" className={cx('text-[0.7em]', !dir && 'invisible')}>
          {dir === 'desc' ? <>&#9660;</> : <>&#9650;</>}
        </span>
      </button>
    </th>
  );
}

// One metadata cell, a skeleton bar while the head loads, a dash on a failed read, and the value once
// present. The skeleton keeps a loading row from collapsing and reads as pending rather than empty.
function PickerCell({ value, failed, muted }: { value: string | null | undefined; failed?: boolean; muted?: boolean }) {
  if (value != null) return <span className={cx('truncate', muted && 'text-muted')}>{value}</span>;
  if (failed) return <span className="text-faint">-</span>;
  return <span className="inline-block h-3 w-16 animate-pulse rounded bg-border" aria-hidden="true" />;
}
