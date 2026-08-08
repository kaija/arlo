/**
 * Radio button — same 18px control as Checkbox, fully round with a 7px dot.
 */
export interface RadioProps extends React.InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  disabled?: boolean;
}
export declare function Radio(props: RadioProps): JSX.Element;
