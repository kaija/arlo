/**
 * Data table in a rounded, clipped card. Header on the section tint.
 */
export interface TableColumn {
  /** Row-object key, also the React key. */
  key: string;
  label: string;
  /** 'right' also switches the cell to mono 13px — use for numbers. */
  align?: 'left' | 'right';
  width?: string | number;
  /** Custom cell renderer, e.g. `row => <Badge>{row.state}</Badge>`. */
  render?: (row: any) => React.ReactNode;
}
export interface TableProps {
  columns?: TableColumn[];
  /** Row objects; `id` is used as the key when present. */
  rows?: any[];
}
export declare function Table(props: TableProps): JSX.Element;
