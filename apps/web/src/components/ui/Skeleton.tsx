export function SkeletonText({ lines = 3 }: { lines?: number }): React.JSX.Element {
  return (
    <div aria-hidden="true">
      {Array.from({ length: lines }, (_, index) => (
        <div
          key={index}
          className="skeleton skeleton-line"
          style={{ width: `${100 - (index % 3) * 14}%` }}
        />
      ))}
    </div>
  );
}

export function SkeletonCard(): React.JSX.Element {
  return <div className="skeleton skeleton-card" aria-hidden="true" />;
}

export function SkeletonTable(): React.JSX.Element {
  return <div className="skeleton skeleton-table" aria-hidden="true" />;
}
