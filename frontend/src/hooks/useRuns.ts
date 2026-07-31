/*
 * Data loading for runs. The run index resolves synchronously from the data directory glob (utils/
 * runIndex), and an individual run is fetched lazily by its served URL. Each document is one zstd
 * frame, decompressed here rather than by the transport, so the host must serve the .json.zst body
 * unencoded. A host that declares a zstd Content-Encoding inflates it before this reader sees it.
 */

import { useEffect, useMemo, useState } from 'react';
import { decompress } from 'fzstd';
import type { Benchmark, RunIndexEntry } from '@/types/benchmark';
import { loadRunIndex } from '@/utils/runIndex';

interface AsyncState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

function useJson<T>(url: string | null): AsyncState<T> {
  const [state, setState] = useState<AsyncState<T>>({ data: null, loading: url !== null, error: null });

  useEffect(() => {
    if (!url) {
      setState({ data: null, loading: false, error: null });
      return;
    }
    const controller = new AbortController();
    setState({ data: null, loading: true, error: null });
    fetch(url, { signal: controller.signal })
      .then(res => {
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
        return res.arrayBuffer();
      })
      .then(buffer => {
        // Decompressing and parsing the largest document costs a couple of seconds, so a switch away
        // mid-flight skips the work rather than paying it for a result no view will read.
        if (controller.signal.aborted) return;
        const json = new TextDecoder().decode(decompress(new Uint8Array(buffer)));
        setState({ data: JSON.parse(json) as T, loading: false, error: null });
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted || (err instanceof DOMException && err.name === 'AbortError')) return;
        setState({ data: null, loading: false, error: err instanceof Error ? err.message : String(err) });
      });
    return () => controller.abort();
  }, [url]);

  return state;
}

// The run index from the data directory, newest first. Memoized since the glob result is static.
export function useRunIndex(): RunIndexEntry[] {
  return useMemo(() => loadRunIndex(), []);
}

export function useRun(entry: RunIndexEntry | null): AsyncState<Benchmark> {
  return useJson<Benchmark>(entry ? entry.url : null);
}
