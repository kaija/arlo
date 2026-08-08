/**
 * Toggle switch — 44×26 pill track (34×20 at sm), accent when on.
 */
export interface SwitchProps extends Omit<React.InputHTMLAttributes<HTMLInputElement>, 'size'> {
  label?: string;
  /** 'sm' = 34×20 track, for dense settings rows. */
  size?: 'sm';
  disabled?: boolean;
}
export declare function Switch(props: SwitchProps): JSX.Element;
