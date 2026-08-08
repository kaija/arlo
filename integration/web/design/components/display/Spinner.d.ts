/**
 * Spinner — 2px ring, accent top arc, 0.7s linear.
 */
export interface SpinnerProps extends React.HTMLAttributes<HTMLSpanElement> {
  /** 14 / 20 (default) / 32 px. */
  size?: 'sm' | 'lg';
  /** White ring, for use inside a primary button. */
  onAccent?: boolean;
  /** Accessible label. Default 'Loading'. */
  label?: string;
}
export declare function Spinner(props: SpinnerProps): JSX.Element;
