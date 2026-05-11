import type { ForwardProbeHop } from "@/lib/api";

export function ProbeTopology({ hops }: { hops: ForwardProbeHop[] }) {
  const NODE_W = 72, NODE_H = 28, H_GAP = 100, V_GAP = 44, PAD = 16;

  const labels = new Map<string, string>();
  for (const h of hops) {
    labels.set(h.from_node, h.from_node_name || h.from_node.slice(0, 8));
    const toId = h.to_node || h.target;
    if (toId) {
      const toLabel = h.to_node_name || (h.target ? h.target.split(":")[0].slice(0, 14) : toId.slice(0, 14));
      labels.set(toId, toLabel);
    }
  }

  const edges = hops.flatMap((h) => {
    const toId = h.to_node || h.target;
    return toId ? [{ from: h.from_node, to: toId, us: h.latency_us, ok: h.ok }] : [];
  });

  const inDegree = new Map<string, number>();
  const adj = new Map<string, string[]>();
  for (const e of edges) {
    inDegree.set(e.to, (inDegree.get(e.to) ?? 0) + 1);
    const list = adj.get(e.from) ?? [];
    list.push(e.to);
    adj.set(e.from, list);
  }
  const layerOf = new Map<string, number>();
  const q = [...labels.keys()].filter((n) => !(inDegree.get(n) ?? 0));
  q.forEach((n) => layerOf.set(n, 0));
  for (let i = 0; i < q.length; i++) {
    const n = q[i];
    for (const next of adj.get(n) ?? []) {
      const l = Math.max(layerOf.get(next) ?? 0, (layerOf.get(n) ?? 0) + 1);
      layerOf.set(next, l);
      if (!q.includes(next)) q.push(next);
    }
  }

  const layers = new Map<number, string[]>();
  for (const [id, l] of layerOf) {
    const arr = layers.get(l) ?? [];
    arr.push(id);
    layers.set(l, arr);
  }
  const numLayers = Math.max(...layerOf.values()) + 1;
  const maxPerLayer = Math.max(...[...layers.values()].map((a) => a.length));

  const svgW = PAD * 2 + numLayers * NODE_W + (numLayers - 1) * H_GAP;
  const svgH = PAD * 2 + maxPerLayer * NODE_H + (maxPerLayer - 1) * V_GAP;

  const pos = new Map<string, { x: number; y: number }>();
  for (const [l, nodes] of layers) {
    const x = PAD + l * (NODE_W + H_GAP);
    const totalH = nodes.length * NODE_H + (nodes.length - 1) * V_GAP;
    const startY = PAD + (svgH - PAD * 2 - totalH) / 2;
    nodes.forEach((id, i) => pos.set(id, { x, y: startY + i * (NODE_H + V_GAP) }));
  }

  const latColor = (us: number) => {
    const ms = us / 1000;
    return ms < 80 ? "#059669" : ms < 200 ? "#d97706" : "#e11d48";
  };

  const allOk = hops.every((h) => h.ok);
  const byTo = new Map<string, number>();
  for (const h of hops) {
    if (!h.ok) continue;
    const key = h.to_node || h.target || h.from_node;
    byTo.set(key, Math.max(byTo.get(key) ?? 0, h.latency_us));
  }
  const total = Array.from(byTo.values()).reduce((s, v) => s + v, 0);
  const totalColor = total / 1000 < 80 ? "#059669" : total / 1000 < 200 ? "#d97706" : "#e11d48";

  return (
    <div className="space-y-2">
      {/* width="100%" + viewBox 让 SVG 自适应容器宽度并保持宽高比，避免窄屏溢出 */}
      <svg
        viewBox={`0 0 ${svgW} ${svgH}`}
        width="100%"
        className="mx-auto overflow-visible"
      >
        {edges.map((e, i) => {
          const f = pos.get(e.from), t = pos.get(e.to);
          if (!f || !t) return null;
          const x1 = f.x + NODE_W, y1 = f.y + NODE_H / 2;
          const x2 = t.x, y2 = t.y + NODE_H / 2;
          const mx = (x1 + x2) / 2;
          const color = e.ok ? latColor(e.us) : "#e11d48";
          const midY = (y1 + y2) / 2;
          return (
            <g key={i}>
              <path
                d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
                fill="none" stroke={color} strokeWidth={1.5} opacity={0.8}
                strokeDasharray={e.ok ? undefined : "4 2"}
              />
              <text
                x={mx} y={midY - 4}
                textAnchor="middle" fontSize={9} fill={color}
                fontFamily="ui-monospace,monospace" fontWeight={600}
              >
                {e.ok ? `${(e.us / 1000).toFixed(1)}ms` : "✗"}
              </text>
            </g>
          );
        })}
        {[...pos.entries()].map(([id, { x, y }]) => (
          <g key={id}>
            <rect x={x} y={y} width={NODE_W} height={NODE_H} rx={6}
              fill="hsl(var(--muted))" stroke="hsl(var(--border))" strokeWidth={1} />
            <text
              x={x + NODE_W / 2} y={y + NODE_H / 2 + 4}
              textAnchor="middle" fontSize={11} fontWeight={500}
              fill="hsl(var(--foreground))"
            >
              {(labels.get(id) ?? id).slice(0, 10)}
            </text>
          </g>
        ))}
      </svg>
      {allOk && hops.length > 0 && (
        <div className="flex items-center justify-between border-t pt-2 text-sm">
          <span className="text-muted-foreground">合计</span>
          <span className="font-mono tabular-nums font-semibold" style={{ color: totalColor }}>
            {(total / 1000).toFixed(1)} ms
          </span>
        </div>
      )}
    </div>
  );
}
