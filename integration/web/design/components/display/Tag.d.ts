/**
 * Chip/tag — user-applied label, optionally removable or clickable as a filter.
 */
export interface TagProps extends React.HTMLAttributes<HTMLElement> {
  /** 'accent' = indigo tint + glow border, for the selected filter state. */
  tone?: 'accent';
  /** Renders a trailing × button. */
  onRemove?: (e: React.MouseEvent) => void;
  /** Makes the whole chip a button (filter toggle). */
  onClick?: (e: React.MouseEvent) => void;
  children?: React.ReactNode;
}
export declare function Tag(props: TagProps): JSX.Element;
