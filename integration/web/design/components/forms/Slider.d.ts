/**
 * Range slider with a filled accent track and mono value readout.
 */
export interface SliderProps extends React.InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  value?: number;
  min?: number;
  max?: number;
  /** Show the mono readout on the right. Default true. */
  showValue?: boolean;
  /** Format the readout, e.g. `v => v.toFixed(1)`. */
  format?: (value: number) => string;
}
export declare function Slider(props: SliderProps): JSX.Element;
