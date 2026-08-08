/**
 * Tooltip — dark 12px label on hover/focus, above by default.
 */
export interface TooltipProps {
  /** Tooltip text — kept on one line, so keep it short. */
  label: string;
  placement?: 'top' | 'bottom';
  children?: React.ReactNode;
}
export declare function Tooltip(props: TooltipProps): JSX.Element;
