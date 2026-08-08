/**
 * Pagination — pill page buttons with chevron arrows and ··· gaps.
 */
export interface PaginationProps {
  /** 1-based current page. */
  page?: number;
  /** Total page count. */
  total?: number;
  onChange?: (page: number) => void;
  /** Leading count text, e.g. "1–20 of 248". */
  meta?: string;
}
export declare function Pagination(props: PaginationProps): JSX.Element;
