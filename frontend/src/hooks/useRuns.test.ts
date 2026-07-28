import { describe, it, expect, vi, afterEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { zstdCompressSync } from 'node:zlib';
import { useRun } from '@/hooks/useRuns';
import { fixture } from '@/test/fixture';

// The single zstd frame the parser writes for a benchmark document, the body the loader fetches.
function buildFrame(document: unknown): ArrayBuffer {
  const bytes = new Uint8Array(zstdCompressSync(Buffer.from(JSON.stringify(document))));
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
}

describe('useRun', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('decompresses a zstd document instead of reading the response as JSON', async () => {
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response(buildFrame(fixture), { status: 200 }));
    const { result } = renderHook(() => useRun({ id: fixture.id, url: '/data/bench.json.zstd' }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeNull();
    expect(result.current.data?.id).toBe(fixture.id);
    expect(result.current.data?.runs.length).toBe(fixture.runs.length);
    expect(result.current.data?.runs[0]?.blocks.length).toBe(fixture.runs[0]?.blocks.length);
    expect(fetchSpy).toHaveBeenCalledTimes(1);
  });

  it('reports a failed response as an error rather than decoding its body', async () => {
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response('', { status: 404, statusText: 'Not Found' }));
    const { result } = renderHook(() => useRun({ id: 'missing', url: '/data/missing.json.zstd' }));

    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.data).toBeNull();
    expect(result.current.error).toBe('404 Not Found');
  });
});
