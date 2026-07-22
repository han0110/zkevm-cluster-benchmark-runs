/*
 * Flat single-stroke line icons so the rail, breadcrumb, and proof panel share one visual language. Most
 * share one 24-unit frame, while the compact filter and dropdown glyphs use their own smaller frames.
 * Each inherits size and color from the caller through className and currentColor, and is aria-hidden
 * because every icon sits beside a text label or an aria-label on its control.
 */

import type { SVGProps } from 'react';
import { cx } from '@/utils/cx';

// Shared frame for every icon, fixing the grid, the flat stroke weight, and the rounded joints once.
function Icon({ className, children, ...rest }: SVGProps<SVGSVGElement>) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={cx('h-5 w-5 shrink-0', className)}
      {...rest}
    >
      {children}
    </svg>
  );
}

// A house, the root of the breadcrumb trail.
export function IconHome(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon viewBox="0 0 24 24" {...props}>
      <path d="M3 11 12 3l9 8" />
      <path d="M5 9.5V21h14V9.5" />
    </Icon>
  );
}

// A small bar chart, the Overview view.
export function IconOverview(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M4 20h16" />
      <path d="M7 20v-7" />
      <path d="M12 20V5" />
      <path d="M17 20v-10" />
    </Icon>
  );
}

// A bulleted list, the Blocks table.
export function IconBlocks(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M9 6h11" />
      <path d="M9 12h11" />
      <path d="M9 18h11" />
      <path d="M4.5 6h.01" />
      <path d="M4.5 12h.01" />
      <path d="M4.5 18h.01" />
    </Icon>
  );
}

// Two stacked server units, the Metrics view.
export function IconNodes(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <rect x="4" y="5" width="16" height="6" rx="1.5" />
      <rect x="4" y="13" width="16" height="6" rx="1.5" />
      <path d="M7.5 8h.01" />
      <path d="M7.5 16h.01" />
    </Icon>
  );
}

// A leftward chevron, the collapse affordance.
export function IconChevronLeft(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M15 6l-6 6 6 6" />
    </Icon>
  );
}

// A rightward chevron, the expand and the panel-close affordance.
export function IconChevronRight(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M9 6l6 6-6 6" />
    </Icon>
  );
}

// Four arrows reaching to the corners, the enter-fullscreen affordance on a chart panel.
export function IconFullscreen(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M8 3H4v4" />
      <path d="M16 3h4v4" />
      <path d="M16 21h4v-4" />
      <path d="M8 21H4v-4" />
    </Icon>
  );
}

// A cross, the close affordance for a fullscreen overlay or dialog.
export function IconClose(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M6 6l12 12" />
      <path d="M18 6L6 18" />
    </Icon>
  );
}

// Two stacked sheets, the copy-to-clipboard affordance.
export function IconCopy(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <rect x="8" y="8" width="13" height="13" rx="2" />
      <path d="M16 8V5a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h3" />
    </Icon>
  );
}

// A check mark, the success cue shown briefly after a copy.
export function IconCheck(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M20 6 9 17l-5-5" />
    </Icon>
  );
}

// Offset thin horizontal bars, a waterfall, the pipeline-fullscreen affordance on a chart panel.
export function IconPipeline(props: SVGProps<SVGSVGElement>) {
  return (
    <Icon {...props}>
      <path d="M3 6h8" />
      <path d="M8 12h8" />
      <path d="M13 18h8" />
    </Icon>
  );
}

// A funnel, the include side of the log filter. Tinted by the caller, green for include. Drawn on its
// own frame at a smaller size than the shared Icon so it sits inside a compact input.
export function IconFilter({ className, ...rest }: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className={cx('h-4 w-4 shrink-0', className)} {...rest}>
      <path d="M21 4H3l7 9v6l4 2v-8z" />
    </svg>
  );
}

// A funnel struck through, the exclude side of the log filter. Tinted red by the caller.
export function IconFilterOff({ className, ...rest }: SVGProps<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" className={cx('h-4 w-4 shrink-0', className)} {...rest}>
      <path d="M21 4H3l7 9v6l4 2v-8z" />
      <path d="M3 3l18 18" />
    </svg>
  );
}

// A small downward chevron, the dropdown's open affordance. It sits on its own 16-unit grid with a
// lighter stroke, so it is drawn outside the shared Icon frame and the caller supplies the className
// that tints and flips it when the menu opens.
export function IconChevronDown({ className, ...rest }: SVGProps<SVGSVGElement>) {
  return (
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true" className={className} {...rest}>
      <path d="M4 6l4 4 4-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}
