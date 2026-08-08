/**
 * Toast — floating white notification with shadow-lg and an optional action.
 */
export interface ToastProps {
  tone?: 'info' | 'success' | 'warning' | 'danger';
  /** Text of the trailing action button, e.g. "Undo". */
  actionLabel?: string;
  onAction?: () => void;
  children?: React.ReactNode;
}
export declare function Toast(props: ToastProps): JSX.Element;
export interface ToastStackProps { children?: React.ReactNode }
/** Fixed bottom-right stack, 10px gap, z-index 300. */
export declare function ToastStack(props: ToastStackProps): JSX.Element;
