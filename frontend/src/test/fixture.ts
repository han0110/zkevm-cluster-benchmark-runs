/*
 * The committed benchmark fixture as the typed Benchmark, the single small document every test reads
 * instead of the multi-megabyte files under data. It carries two runs, a block missing a node, a crashed
 * block, a block with fine-pipeline rows, and a block id holding a single quote, so the tests exercise
 * the multi-run combine, the latest-attempt logic, the participating-node range, the pipeline decode,
 * and the shell quoting against one shape.
 *
 * JSON widens the status union and the phase tuples to bare string and array, so the document is asserted
 * to the exact Benchmark shape once here for every test to import.
 */

import type { Benchmark } from '@/types/benchmark';
import benchmark from '@/test/fixtures/benchmark.json';

export const fixture = benchmark as unknown as Benchmark;
