import { describe, it, expect } from 'vitest';
import { archiveRelPath, blockArchivePath } from '@/utils/archivePath';

describe('blockArchivePath', () => {
  it('keeps a single-segment name as one round-trippable file name', () => {
    const name = 'rpc_block_24949867';
    expect(blockArchivePath(name)).toBe(name);
    expect(blockArchivePath(name)).not.toContain('/');
  });

  it('flattens an EEST id to a single colon-free file name using __ for the separator', () => {
    const name = 'test_block.py::test_call[fork_Prague-state_test]';
    const flat = blockArchivePath(name);
    expect(flat).toBe('test_block.py__test_call%5Bfork_Prague-state_test%5D');
    expect(flat).not.toContain('/'); // flat: no directory separator
    expect(flat).not.toContain('%3A'); // no colon survives to the served path
    expect(flat).toContain('__'); // the :: became __
    expect(decodeURIComponent(flat)).toBe('test_block.py__test_call[fork_Prague-state_test]');
  });

  it('flattens the fixture path of an EEST v0.6.2 id using _ for the separators', () => {
    const name =
      'tests/benchmark/compute/instruction/test_stack.py::test_swap[fork_Amsterdam-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M]';
    const flat = blockArchivePath(name);
    expect(flat).toBe(
      'tests_benchmark_compute_instruction_test_stack.py__test_swap%5Bfork_Amsterdam-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M%5D'
    );
    expect(flat).not.toContain('%2F'); // no encoded separator survives to the served path
  });
});

describe('archiveRelPath', () => {
  it('joins the benchmark, run, and block under a flat .tar.json file', () => {
    expect(archiveRelPath('mainnet-1', 'mainnet-1', 'rpc_block_24949867')).toBe(
      'mainnet-1/mainnet-1/rpc_block_24949867.tar.json'
    );
  });

  it('maps an EEST id to a flat encoded archive path directly under the run', () => {
    const rel = archiveRelPath(
      'eest-60m',
      'eest-60m-run',
      'test_stack.py::test_swap[fork_Osaka-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M]'
    );
    expect(rel).toBe(
      'eest-60m/eest-60m-run/test_stack.py__test_swap%5Bfork_Osaka-blockchain_test-opcode_SWAP15-benchmark-gas-value_60M%5D.tar.json'
    );
  });

  it('encodes the flat on-disk path the same way the build-time index does', () => {
    // The index encodes each on-disk path segment (the benchmark, the run, and the single flat file name
    // including its .tar.json suffix) with encodeURIComponent. archiveRelPath must produce that identical
    // string so the presence check finds the file.
    const name =
      'tests/benchmark/compute/precompile/test_alt_bn128.py::test_alt_bn128[fork_Osaka-blockchain_test-bn128_add-benchmark-gas-value_60M]';
    const rel = archiveRelPath('b', 'r', name);
    const flatFile = `${name.split('::').join('__').split('/').join('_')}.tar.json`;
    const encoded = ['b', 'r', flatFile].map(encodeURIComponent).join('/');
    expect(rel).toBe(encoded);
  });
});
