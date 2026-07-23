import { describe, it, expect } from 'vitest';
import { linearFit, predict, relativeResidual } from '@/utils/trendFit';

describe('linearFit', () => {
  it('recovers the slope and intercept of points on an exact line', () => {
    // y = 2 + 3x, so every residual is zero.
    const points: Array<[number, number]> = [
      [0, 2],
      [1, 5],
      [2, 8],
      [3, 11],
    ];
    const fit = linearFit(points);
    expect(fit).not.toBeNull();
    expect(fit!.slope).toBeCloseTo(3);
    expect(fit!.intercept).toBeCloseTo(2);
    for (const pt of points) expect(relativeResidual(fit!, pt)).toBeCloseTo(0);
  });

  it('returns signed relative residuals for a vertically offset dataset', () => {
    // A horizontal fit at y = 10 with points pushed one unit above and below, so the residuals are the
    // known plus and minus ten percent offsets.
    const fit = linearFit([
      [0, 11],
      [1, 9],
      [2, 9],
      [3, 11],
    ]);
    expect(fit).not.toBeNull();
    expect(fit!.slope).toBeCloseTo(0);
    expect(fit!.intercept).toBeCloseTo(10);
    expect(relativeResidual(fit!, [0, 11])).toBeCloseTo(0.1);
    expect(relativeResidual(fit!, [1, 9])).toBeCloseTo(-0.1);
    expect(relativeResidual(fit!, [3, 11])).toBeCloseTo(0.1);
  });

  it('returns null for degenerate inputs rather than crashing or dividing by zero', () => {
    expect(linearFit([])).toBeNull();
    expect(linearFit([[5, 3]])).toBeNull();
    // Zero-variance x, every block at the same gas, leaves the slope undetermined.
    expect(
      linearFit([
        [2, 1],
        [2, 4],
        [2, 9],
      ])
    ).toBeNull();
  });

  it('yields a zero residual when the prediction is non-positive', () => {
    expect(relativeResidual({ slope: 1, intercept: 0 }, [0, 5])).toBe(0);
    expect(relativeResidual({ slope: 1, intercept: -10 }, [0, 5])).toBe(0);
  });
});

describe('predict', () => {
  it('evaluates the fitted line at a point', () => {
    expect(predict({ slope: 2, intercept: 1 }, 3)).toBe(7);
  });
});
