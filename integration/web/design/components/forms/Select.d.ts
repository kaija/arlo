/**
 * Native select with the brand chevron. Same field chrome as Input.
 */
export interface SelectProps extends Omit<React.SelectHTMLAttributes<HTMLSelectElement>, 'size'> {
  label?: string;
  hint?: string;
  error?: string;
  /** Options as plain strings or `{ value, label }` objects. */
  options?: Array<string | { value: string; label: string }>;
  size?: 'sm';
}
export declare function Select(props: SelectProps): JSX.Element;
