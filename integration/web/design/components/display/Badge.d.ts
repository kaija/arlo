/**
 * Status badge — 12px semibold on a tinted pill.
 */
export interface BadgeProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** Default 'accent'. */
  tone?: 'accent' | 'neutral' | 'success' | 'warning' | 'danger' | 'solid';
  /** Leading 6px dot in the current tone colour. */
  dot?: boolean;
  children?: React.ReactNode;
}
export declare function Badge(props: BadgeProps): JSX.Element;
