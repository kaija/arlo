/**
 * Inline alert — tinted 12px-radius panel with a status icon.
 */
export interface AlertProps {
  /** Default 'info' (indigo). */
  tone?: 'info' | 'success' | 'warning' | 'danger';
  title?: string;
  /** Renders a dismiss ×. */
  onClose?: () => void;
  children?: React.ReactNode;
}
export declare function Alert(props: AlertProps): JSX.Element;
