/**
 * Progress bar — 6px pill track, accent fill, optional label/percent head.
 */
export interface ProgressProps {
  value?: number;
  /** Default 100. */
  max?: number;
  label?: string;
  tone?: 'success' | 'danger';
  /** Animated 35% sweep for unknown duration; hides the percent. */
  indeterminate?: boolean;
  /** Show the mono percent on the right. Default true. */
  showValue?: boolean;
}
export declare function Progress(props: ProgressProps): JSX.Element;
