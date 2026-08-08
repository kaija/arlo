/** Single-row footer: 24px mark + wordmark left, tertiary link row right, top hairline border. */
export interface FooterProps {
  links?: { label: string; href: string }[];
}
export declare function Footer(props: FooterProps): JSX.Element;
