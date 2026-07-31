import { describe, it, expect, vi, afterEach } from 'vitest';
import { zstdCompressSync } from 'node:zlib';
import { loadBenchmarkMeta, parseBenchmarkHead } from '@/utils/benchmarkMeta';

// A compact head shaped like a real document, truncated partway through the first run as a ranged read
// returns it.
const head = (extra = '') =>
  `{"schema_version":1,"hardware":{"cpu_model":"AMD","ram_gib":125,"gpu_models":["RTX 5090"],"nodes":["node1"]},` +
  `"software":{"zkvm":{"name":"zisk","version":"v0.18.0","phases":[{"name":"input","label":"Input Transfer"}]},` +
  `"guest":{"name":"reth","version":"v2.1.0"}},"id":"eest-60m-20260603-002355","name":"eest-60m",` +
  `"description":"EEST blocks with 60M gas limit"${extra},"runs":[{"id":"eest-60m-20260603-002355","started_at":1748910235000,"block_count":1077`;

describe('parseBenchmarkHead', () => {
  it('extracts identity, software, and the first run start from a truncated head', () => {
    const meta = parseBenchmarkHead(head());
    expect(meta.id).toBe('eest-60m-20260603-002355');
    expect(meta.name).toBe('eest-60m');
    expect(meta.description).toBe('EEST blocks with 60M gas limit');
    expect(meta.software.zkvm.name).toBe('zisk');
    expect(meta.software.zkvm.version).toBe('v0.18.0');
    expect(meta.software.guest.name).toBe('reth');
    expect(meta.software.guest.version).toBe('v2.1.0');
    expect(meta.startedAt).toBe(1748910235000);
  });

  it('parses a complete body with an empty runs array, reporting no start', () => {
    const body =
      `{"schema_version":1,"hardware":{"cpu_model":null,"ram_gib":null,"gpu_models":[],"nodes":[]},` +
      `"software":{"zkvm":{"name":"zisk","version":"v0.18.0","phases":[]},"guest":{"name":"reth","version":"v2.1.0"}},` +
      `"id":"empty-bench","name":"empty","description":"no runs","runs":[]}`;
    const meta = parseBenchmarkHead(body);
    expect(meta.id).toBe('empty-bench');
    expect(meta.startedAt).toBeNull();
  });

  it('truncates at the runs boundary so a long runs payload never needs parsing', () => {
    // Trailing junk after the boundary stands in for the multi-megabyte runs array the reader never sees.
    const meta = parseBenchmarkHead(head() + ',{"id":"second-run","started_at":9}]}garbage-not-valid-json');
    // The first run's start wins, and the unparseable tail is ignored because the object closes at runs.
    expect(meta.startedAt).toBe(1748910235000);
    expect(meta.name).toBe('eest-60m');
  });

  it('parses a whitespace-formatted head, tolerating spacing around the runs key and colon', () => {
    // A pretty-printed document with newlines and spaces around every key, colon, and the runs boundary,
    // truncated partway through the first run as a ranged read returns it.
    const pretty =
      `{\n  "schema_version": 1,\n  "hardware": {"cpu_model": "AMD", "ram_gib": 125, "gpu_models": ["RTX 5090"], "nodes": ["node1"]},\n` +
      `  "software": {"zkvm": {"name": "zisk", "version": "v0.18.0", "phases": []}, "guest": {"name": "reth", "version": "v2.1.0"}},\n` +
      `  "id": "pretty-bench",\n  "name": "pretty",\n  "description": "whitespace formatted" ,\n` +
      `  "runs" : [\n    {"id": "pretty-bench", "started_at" : 1748910235000, "block_count": 1077`;
    const meta = parseBenchmarkHead(pretty);
    expect(meta.id).toBe('pretty-bench');
    expect(meta.name).toBe('pretty');
    expect(meta.description).toBe('whitespace formatted');
    expect(meta.software.zkvm.name).toBe('zisk');
    expect(meta.startedAt).toBe(1748910235000);
  });
});

// The byte count the reader requests, mirrored here so the truncation matches a real ranged read.
const HEAD_BYTES = 1 << 18;

const ALPHABET = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/';

// Filler from a deterministic xorshift over a wide alphabet, compressing at a ratio near a real
// document's rather than collapsing into back-references, which keeps the frame long enough for the head
// to cut it partway.
function filler(length: number): string {
  let text = '';
  let state = 2463534242;
  for (let i = 0; i < length; i += 1) {
    state ^= state << 13;
    state >>>= 0;
    state ^= state >>> 17;
    state ^= state << 5;
    state >>>= 0;
    text += ALPHABET[state >>> 26];
  }
  return text;
}

// The body of one fetch, copied so the Response owns a standalone buffer.
const body = (bytes: Uint8Array): ArrayBuffer => bytes.slice().buffer;

describe('loadBenchmarkMeta', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('decodes the metadata from a frame the ranged read cut partway', async () => {
    const document = `${head()},"pad":"${filler(600_000)}"}]}`;
    const frame = new Uint8Array(zstdCompressSync(Buffer.from(document)));
    // A head carrying no end-of-frame marker is what the decoder has to survive, since the range stops
    // wherever the byte count lands.
    expect(frame.length).toBeGreaterThan(HEAD_BYTES);
    const truncated = frame.subarray(0, HEAD_BYTES);
    const fetchSpy = vi
      .spyOn(globalThis, 'fetch')
      .mockResolvedValue(new Response(body(truncated), { status: 206 }));

    const meta = await loadBenchmarkMeta('/data/truncated.json.zst');
    expect(meta.id).toBe('eest-60m-20260603-002355');
    expect(meta.name).toBe('eest-60m');
    expect(meta.software.zkvm.name).toBe('zisk');
    expect(meta.startedAt).toBe(1748910235000);
    // One ranged read is what keeps the picker off the whole multi-megabyte document.
    expect(fetchSpy).toHaveBeenCalledTimes(1);
    expect(fetchSpy.mock.calls[0]?.[1]).toMatchObject({ headers: { Range: expect.stringMatching(/^bytes=0-/) } });
  });

  it('decodes a document whose whole frame fits inside the range', async () => {
    const frame = new Uint8Array(zstdCompressSync(Buffer.from(`${head()}}]}`)));
    vi.spyOn(globalThis, 'fetch').mockResolvedValue(new Response(body(frame), { status: 200 }));

    const meta = await loadBenchmarkMeta('/data/whole.json.zst');
    expect(meta.id).toBe('eest-60m-20260603-002355');
    expect(meta.startedAt).toBe(1748910235000);
  });
});
