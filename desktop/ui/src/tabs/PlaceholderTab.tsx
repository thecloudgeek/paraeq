// A stub for the not-yet-built tabs. Each names the stage it lands in so the
// shell reads honestly before Tasks 12-17 fill the panels.

export function PlaceholderTab({ stage, title }: { stage: string; title: string }) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 p-8 text-center text-muted-foreground">
      <h2 className="text-lg font-medium text-foreground">{title}</h2>
      <p className="text-sm">Coming in {stage}</p>
    </div>
  );
}
