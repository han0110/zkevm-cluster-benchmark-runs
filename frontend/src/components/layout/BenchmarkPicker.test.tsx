import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import type { BenchmarkMeta } from '@/utils/benchmarkMeta';
import { BenchmarkPicker } from '@/components/layout/BenchmarkPicker';

// Three benchmarks with distinct times and distinct software labels, so every ordering has one answer
// and no tie leaves an assertion resting on sort stability.
const ROWS = vi.hoisted(() => [
  { id: 'old-zisk', url: '/data/old-zisk.json.zstd', guest: 'reth@v2.1.0', zkvm: 'zisk@v0.18.0', startedAt: 1_000 },
  { id: 'new-openvm', url: '/data/new-openvm.json.zstd', guest: 'ethrex@v15.0.0', zkvm: 'openvm@9a00000', startedAt: 3_000 },
  { id: 'mid-openvm', url: '/data/mid-openvm.json.zstd', guest: 'reth@c5dff62', zkvm: 'openvm@8f86342', startedAt: 2_000 },
]);

// The picker reads each entry's metadata head through loadBenchmarkMeta. The test stands in a resolved
// meta so the table fills without a network fetch.
vi.mock('@/utils/benchmarkMeta', () => ({
  loadBenchmarkMeta: (url: string): Promise<BenchmarkMeta> => {
    const row = ROWS.find(r => r.url === url)!;
    const [guestName, guestVersion] = row.guest.split('@');
    const [zkvmName, zkvmVersion] = row.zkvm.split('@');
    return Promise.resolve({
      id: row.id,
      name: `Name for ${row.id}`,
      description: `Description for ${row.id}`,
      software: {
        zkvm: { name: zkvmName!, version: zkvmVersion!, phases: [] },
        guest: { name: guestName!, version: guestVersion! },
      },
      startedAt: row.startedAt,
    });
  },
}));

const entries = ROWS.map(({ id, url }) => ({ id, url }));

// The displayed order, read from each row's id cell.
const rowIds = (dialog: HTMLElement): string[] =>
  [...dialog.querySelectorAll('tbody tr')].map(r => r.querySelector('td:last-child')!.textContent!.trim());

// The column labels, with the up and down sort indicators a sortable header carries dropped.
const headerLabels = (dialog: HTMLElement): string[] =>
  [...dialog.querySelectorAll('thead th')].map(h => h.textContent!.replace(/[\u25B2\u25BC]/g, '').trim());

async function openPicker(onSelect: (id: string) => void = () => {}): Promise<HTMLElement> {
  render(<BenchmarkPicker entries={entries} selectedId="old-zisk" selectedName="eest-60m" onSelect={onSelect} />);
  fireEvent.click(screen.getByRole('button', { name: 'Select benchmark' }));
  const dialog = screen.getByRole('dialog', { name: 'Select benchmark' });
  await waitFor(() => expect(dialog.textContent).toContain('zisk@v0.18.0'));
  return dialog;
}

describe('BenchmarkPicker', () => {
  it('shows the selected name on the trigger and the id when no name is known', () => {
    const { rerender } = render(
      <BenchmarkPicker entries={entries} selectedId="old-zisk" selectedName="eest-60m" onSelect={() => {}} />
    );
    expect(screen.getByRole('button', { name: 'Select benchmark' }).textContent).toContain('eest-60m');
    rerender(<BenchmarkPicker entries={entries} selectedId="old-zisk" selectedName={null} onSelect={() => {}} />);
    expect(screen.getByRole('button', { name: 'Select benchmark' }).textContent).toContain('old-zisk');
  });

  it('lists the columns in reading order and drops the name', async () => {
    const dialog = await openPicker();
    expect(headerLabels(dialog)).toEqual(['Benchmark at', 'Guest', 'zkVM', 'Description', 'ID']);
    expect(dialog.textContent).not.toContain('Name for old-zisk');
  });

  it('opens newest first and loads each row metadata', async () => {
    const dialog = await openPicker();
    expect(rowIds(dialog)).toEqual(['new-openvm', 'mid-openvm', 'old-zisk']);
    // The merged software cells read name and version together once the head resolves.
    expect(dialog.textContent).toContain('reth@v2.1.0');
    expect(dialog.textContent).toContain('openvm@8f86342');
  });

  it('sorts by guest and by zkVM, flipping the direction on a second press', async () => {
    const dialog = await openPicker();

    fireEvent.click(screen.getByRole('button', { name: /^Guest/ }));
    expect(rowIds(dialog)).toEqual(['new-openvm', 'mid-openvm', 'old-zisk']);
    fireEvent.click(screen.getByRole('button', { name: /^Guest/ }));
    expect(rowIds(dialog)).toEqual(['old-zisk', 'mid-openvm', 'new-openvm']);

    fireEvent.click(screen.getByRole('button', { name: /^zkVM/ }));
    expect(rowIds(dialog)).toEqual(['mid-openvm', 'new-openvm', 'old-zisk']);
  });

  it('marks the sorted column and flips to oldest first on a second press of the time header', async () => {
    const dialog = await openPicker();
    const timeHeader = dialog.querySelector('thead th')!;
    expect(timeHeader.getAttribute('aria-sort')).toBe('descending');

    fireEvent.click(screen.getByRole('button', { name: /^Benchmark at/ }));
    expect(timeHeader.getAttribute('aria-sort')).toBe('ascending');
    expect(rowIds(dialog)).toEqual(['old-zisk', 'mid-openvm', 'new-openvm']);
  });

  it('marks the open benchmark and reports the row a press chooses', async () => {
    const onSelect = vi.fn();
    const dialog = await openPicker(onSelect);

    // The open benchmark is the oldest, which the newest-first default puts last.
    const rows = [...dialog.querySelectorAll('tbody tr')];
    expect(rows[2]!.getAttribute('aria-selected')).toBe('true');

    fireEvent.click(rows[0]!);
    expect(onSelect).toHaveBeenCalledWith('new-openvm');
    expect(screen.queryByRole('dialog')).toBeNull();
  });
});
