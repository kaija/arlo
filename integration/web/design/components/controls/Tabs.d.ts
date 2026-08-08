/**
 * Tabs — underline bar, or a pill segmented control matching the language switch.
 */
export interface TabItem {
  id: string;
  label: string;
  /** Optional trailing count, e.g. 12. */
  count?: number;
}
export interface TabsProps {
  tabs?: Array<string | TabItem>;
  /** Selected tab id. */
  value?: string;
  onChange?: (id: string) => void;
  /** 'pill' = segmented control on the sunken tint. Default is the underline bar. */
  variant?: 'pill';
}
export declare function Tabs(props: TabsProps): JSX.Element;
