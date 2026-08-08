/**
 * Modal dialog — 460px card on a blurred indigo-black scrim.
 */
export interface DialogProps {
  /** Default true; render nothing when false. */
  open?: boolean;
  title?: string;
  /** Optional `Icon` name shown in a 36px accent-tint box. */
  icon?: string;
  /** Called by the × and by clicking the scrim. */
  onClose?: () => void;
  /** Footer buttons, right-aligned. */
  actions?: React.ReactNode;
  children?: React.ReactNode;
}
export declare function Dialog(props: DialogProps): JSX.Element;
