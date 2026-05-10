interface Props {
  brand?: string;
}

export default function BootSplash({ brand }: Props) {
  return (
    <div className="fixed inset-0 z-[100] flex flex-col items-center justify-center gap-6 bg-background">
      <svg width="56" height="24" viewBox="0 0 56 24" fill="none" aria-hidden>
        {([8, 28, 48] as const).map((cx, i) => (
          <circle key={cx} cx={cx} cy="16" r="4" fill="hsl(199 78% 44%)">
            <animateTransform
              attributeName="transform"
              type="translate"
              values="0,0;0,-8;0,0"
              dur="1.2s"
              begin={`${i * 0.2}s`}
              repeatCount="indefinite"
              calcMode="spline"
              keyTimes="0;0.5;1"
              keySplines="0.4 0 0.6 1;0.4 0 0.6 1"
            />
          </circle>
        ))}
      </svg>
      {brand && (
        <p className="text-xs font-semibold uppercase tracking-[0.25em] text-muted-foreground">
          {brand}
        </p>
      )}
    </div>
  );
}
