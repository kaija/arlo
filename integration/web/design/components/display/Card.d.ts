/**
 * Surface container — 16px radius, hairline border, 32/28 padding.
 */
export interface CardProps extends React.HTMLAttributes<HTMLDivElement> {
  title?: string;
  subtitle?: string;
  /** Right-aligned slot in the header — a Badge, icon button or menu. */
  action?: React.ReactNode;
  /** Footer content above a top hairline. */
  footer?: React.ReactNode;
  /** 'sm' = 20px padding, for dense grids and sidebars. */
  padding?: 'sm';
  /** Adds the brand hover lift (glow border, md shadow + ring, −2px). Only for clickable cards. */
  interactive?: boolean;
  children?: React.ReactNode;
}
export declare function Card(props: CardProps): JSX.Element;
