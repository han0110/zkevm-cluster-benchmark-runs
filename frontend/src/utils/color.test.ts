import { describe, it, expect } from 'vitest';
import { tints } from '@/utils/color';

describe('tints', () => {
  it('spreads a base color into distinct hex shades, lightest first', () => {
    const ramp = tints('#3b82f6', 7);
    expect(ramp).toHaveLength(7);
    expect(ramp.every(c => /^#[0-9a-f]{6}$/.test(c))).toBe(true);
    // Every shade is distinct, and the ramp runs from lightest to darkest.
    expect(new Set(ramp).size).toBe(7);
    const luminance = (hex: string) => parseInt(hex.slice(1, 3), 16) + parseInt(hex.slice(3, 5), 16) + parseInt(hex.slice(5, 7), 16);
    expect(luminance(ramp[0]!)).toBeGreaterThan(luminance(ramp[6]!));
  });

  it('returns the base unchanged for a single shade and empty for none', () => {
    expect(tints('#3b82f6', 1)).toEqual(['#3b82f6']);
    expect(tints('#3b82f6', 0)).toEqual([]);
  });
});
