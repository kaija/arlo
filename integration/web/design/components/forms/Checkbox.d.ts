/**
 * Checkbox with 18px box, 5px radius, accent fill and a 12px tick.
 */
export interface CheckboxProps extends React.InputHTMLAttributes<HTMLInputElement> {
  /** Label to the right of the box. Sentence case. */
  label?: string;
  disabled?: boolean;
}
export declare function Checkbox(props: CheckboxProps): JSX.Element;
