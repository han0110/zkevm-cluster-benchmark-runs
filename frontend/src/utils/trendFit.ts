/*
 * Ordinary least-squares line fit and relative residuals for the gas-vs-time scatter. The fit finds
 * proving_seconds = intercept + slope * gas over the plotted points, and each point's relative residual
 * measures how far its actual time sits above or below the line as a fraction of the predicted time.
 */

export interface LinearFit {
  slope: number;
  intercept: number;
}

// Ordinary least-squares fit of y on x, returning null when fewer than two points or a zero-variance x
// leaves the slope undetermined.
export function linearFit(points: Array<[number, number]>): LinearFit | null {
  const n = points.length;
  if (n < 2) return null;
  const sx = points.reduce((a, [x]) => a + x, 0);
  const sy = points.reduce((a, [, y]) => a + y, 0);
  const sxx = points.reduce((a, [x]) => a + x * x, 0);
  const sxy = points.reduce((a, [x, y]) => a + x * y, 0);
  const denom = n * sxx - sx * sx;
  if (denom === 0) return null;
  const slope = (n * sxy - sx * sy) / denom;
  const intercept = (sy - slope * sx) / n;
  return { slope, intercept };
}

// Predicted y on the fitted line at x.
export const predict = (fit: LinearFit, x: number): number => fit.intercept + fit.slope * x;

// Relative residual of a point against the fit, (actual - predicted) / predicted, the signed fraction the
// actual time runs above or below the trend. Returns 0 when the predicted time is non-positive so a
// degenerate fit never divides by zero.
export function relativeResidual(fit: LinearFit, [x, y]: [number, number]): number {
  const predicted = predict(fit, x);
  return predicted > 0 ? (y - predicted) / predicted : 0;
}
