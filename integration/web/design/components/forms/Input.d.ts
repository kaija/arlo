/**
 * Single-line or multiline text field with label, hint, error and optional leading icon.
 */
export interface InputProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'size'> {
  /** Label above the field. Sentence case, no trailing colon. */
  label?: string;
  /** Helper text under the field. Replaced by `error` when set. */
  hint?: string;
  /** Error message — also turns the border and hint red. */
  error?: string;
  /** Leading `Icon` name (e.g. 'search'), rendered at 16px inside the field. */
  icon?: string;
  /** 'sm' = 7px/11px padding, 13px text. Default is 10px/14px, 15px text. */
  size?: 'sm';
  /** Render a textarea instead (min-height 88px, vertical resize). */
  as?: 'textarea';
}
export declare function Input(props: InputProps): JSX.Element;
