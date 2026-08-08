/** Principle/feature card: 44px accent-tinted icon box, 16px title, 14px secondary body. Lifts 2px on hover. */
export interface FeatureCardProps {
  /** 24px stroke icon (currentColor renders accent) */
  icon?: React.ReactNode;
  title: React.ReactNode;
  children?: React.ReactNode;
}
export declare function FeatureCard(props: FeatureCardProps): JSX.Element;
