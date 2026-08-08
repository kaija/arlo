/**
 * Pill-shaped button. Primary = solid accent; secondary = white with border; sm = compact nav-height outline.
 */
export interface ButtonProps {
  /** 'primary' solid #5856D6 · 'secondary' white/bordered · omit with size 'sm' for the nav GitHub button */
  variant?: 'primary' | 'secondary';
  /** 'sm' = 6px 14px padding, 13px text, bordered */
  size?: 'sm';
  /** Renders an <a> (external hrefs get target=_blank rel=noopener) instead of <button> */
  href?: string;
  children?: React.ReactNode;
  onClick?: () => void;
}
export declare function Button(props: ButtonProps): JSX.Element;
