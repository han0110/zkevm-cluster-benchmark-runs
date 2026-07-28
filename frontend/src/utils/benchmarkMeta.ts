/*
 * Lightweight metadata reader for the benchmark picker, reading only the small leading head of a flat
 * data/{id}.json.zstd with a ranged request so listing never parses the multi-megabyte runs array. The
 * head is a truncated zstd frame, decompressed by a streaming reader that stops where the range cut it
 * off.
 */

import { Decompress } from 'fzstd';
import type { Software } from '@/types/benchmark';

// The identity, software, and start a picker row shows, read from a benchmark document's head.
export interface BenchmarkMeta {
  id: string;
  name: string;
  description: string;
  software: Software;
  // The earliest run's start as unix epoch milliseconds, or null for a document carrying no run.
  startedAt: number | null;
}

// The compressed head byte count requested. The decoder hands back text in 128 KiB units, so the head
// carries enough compressed bytes to fill one, which 256 KiB always does because a zstd block never
// expands its input. A frame that fits inside the range arrives whole and decodes in full instead. The
// metadata preceding the runs array is a few hundred bytes, so it lands in the first unit either way.
const HEAD_BYTES = 1 << 18;

// The pattern that opens the runs array, the boundary the metadata head is truncated at. The keys before
// it are the small identity and software fields, so closing the object here yields valid JSON. Optional
// whitespace around the comma, the quoted key, and the colon is tolerated so a pretty-printed document
// still finds the boundary while the minified fast path still matches.
const RUNS_BOUNDARY = /,\s*"runs"\s*:\s*\[/;

const cache = new Map<string, Promise<BenchmarkMeta>>();

// Reads a benchmark document's metadata head, fetching only the leading bytes. The result is cached per
// url, and a rejected fetch is evicted so a later open retries instead of resolving the prior failure.
export function loadBenchmarkMeta(url: string): Promise<BenchmarkMeta> {
  const cached = cache.get(url);
  if (cached) return cached;
  const pending = fetchMeta(url);
  cache.set(url, pending);
  pending.catch(() => cache.delete(url));
  return pending;
}

// Parses a benchmark document head into its metadata, lifting the first run's start out by pattern. The
// runs boundary is required when the head is a truncated ranged read, since the buffer past it is a
// guaranteed-incomplete runs array that JSON.parse would reject. A head with no boundary is only valid
// when it is a complete body, recognized by a closing brace, so anything else fails gracefully rather
// than parsing a truncated buffer.
export function parseBenchmarkHead(head: string): BenchmarkMeta {
  const match = head.match(RUNS_BOUNDARY);
  let metaJson: string;
  if (match && match.index != null) {
    metaJson = `${head.slice(0, match.index)}}`;
  } else if (head.trimEnd().endsWith('}')) {
    metaJson = head;
  } else {
    throw new Error('benchmark head carries no runs boundary and is not a complete document');
  }
  const obj = JSON.parse(metaJson) as Pick<BenchmarkMeta, 'id' | 'name' | 'description' | 'software'>;
  const started = head.match(/"started_at"\s*:\s*(\d+)/);
  return {
    id: obj.id,
    name: obj.name,
    description: obj.description,
    software: obj.software,
    startedAt: started ? Number(started[1]) : null,
  };
}

// Decompresses the leading text of a benchmark document from a zstd frame head. A truncated frame is the
// normal case, and the streaming reader yields the text it has already decoded before simply stopping, so
// no end-of-frame marker is needed. One streaming TextDecoder spans the decoded chunks, keeping a
// multi-byte character split across a chunk boundary intact.
function decodeHead(frame: Uint8Array): string {
  const decoder = new TextDecoder();
  let head = '';
  const stream = new Decompress(chunk => {
    head += decoder.decode(chunk, { stream: true });
  });
  stream.push(frame, false);
  return head;
}

async function fetchMeta(url: string): Promise<BenchmarkMeta> {
  // The ranged request returns the head on a host that honors it (the dev server and GitHub Pages both
  // do). A host that ignores the range returns the whole body, which still parses correctly from the
  // same head slice, only without the transfer saving.
  const res = await fetch(url, { headers: { Range: `bytes=0-${HEAD_BYTES - 1}` } });
  if (!res.ok && res.status !== 206) throw new Error(`${res.status} ${res.statusText}`);
  return parseBenchmarkHead(decodeHead(new Uint8Array(await res.arrayBuffer())));
}
