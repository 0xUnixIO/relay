interface TunnelTopologyProps {
  layers: string[][];
  nodeNames: Record<string, string>;
}

const NODE_W = 96, NODE_H = 32, H_GAP = 80, V_GAP = 12, PAD = 16;

/** 超过 12 字符时截断加省略号 */
function truncate(s: string): string {
  return s.length > 12 ? s.slice(0, 12) + "…" : s;
}

export function TunnelTopology({ layers, nodeNames }: TunnelTopologyProps) {
  if (layers.length === 0) {
    return (
      <div className="text-sm text-muted-foreground text-center py-4">
        暂无节点信息
      </div>
    );
  }

  // 计算 SVG 画布尺寸
  const maxPerLayer = Math.max(...layers.map((l) => l.length));
  const svgW = PAD * 2 + layers.length * NODE_W + (layers.length - 1) * H_GAP;
  const svgH = PAD * 2 + maxPerLayer * NODE_H + (maxPerLayer - 1) * V_GAP;

  // 计算每个节点的坐标（左上角）
  const pos = new Map<string, { x: number; y: number }>();
  layers.forEach((nodeIds, layerIdx) => {
    const x = PAD + layerIdx * (NODE_W + H_GAP);
    const totalH = nodeIds.length * NODE_H + (nodeIds.length - 1) * V_GAP;
    const startY = PAD + (svgH - PAD * 2 - totalH) / 2;
    nodeIds.forEach((id, nodeIdx) => {
      pos.set(id, { x, y: startY + nodeIdx * (NODE_H + V_GAP) });
    });
  });

  // 构建相邻层之间的连线：当前层每个节点 → 下一层每个节点
  const edges: Array<{ from: string; to: string }> = [];
  for (let i = 0; i < layers.length - 1; i++) {
    for (const fromId of layers[i]) {
      for (const toId of layers[i + 1]) {
        edges.push({ from: fromId, to: toId });
      }
    }
  }

  return (
    <svg
      viewBox={`0 0 ${svgW} ${svgH}`}
      width="100%"
      className="mx-auto overflow-visible"
    >
      {/* 连线：相邻层之间的三次贝塞尔曲线 */}
      {edges.map((e, i) => {
        const f = pos.get(e.from);
        const t = pos.get(e.to);
        if (!f || !t) return null;
        const x1 = f.x + NODE_W;
        const y1 = f.y + NODE_H / 2;
        const x2 = t.x;
        const y2 = t.y + NODE_H / 2;
        const mx = (x1 + x2) / 2;
        return (
          <path
            key={i}
            d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
            fill="none"
            stroke="hsl(var(--muted-foreground))"
            strokeWidth={1.5}
            opacity={0.5}
          />
        );
      })}

      {/* 节点框 */}
      {[...pos.entries()].map(([id, { x, y }]) => {
        const label = truncate(nodeNames[id] ?? id.slice(0, 12));
        return (
          <g key={id}>
            <rect
              x={x}
              y={y}
              width={NODE_W}
              height={NODE_H}
              rx={6}
              fill="hsl(var(--muted))"
              stroke="hsl(var(--border))"
              strokeWidth={1}
            />
            <text
              x={x + NODE_W / 2}
              y={y + NODE_H / 2 + 4}
              textAnchor="middle"
              fontSize={11}
              fontWeight={500}
              fill="hsl(var(--foreground))"
            >
              {label}
            </text>
          </g>
        );
      })}

      {/* 每层上方的 Hop 编号标签 */}
      {layers.map((_, layerIdx) => {
        const x = PAD + layerIdx * (NODE_W + H_GAP) + NODE_W / 2;
        return (
          <text
            key={layerIdx}
            x={x}
            y={PAD - 4}
            textAnchor="middle"
            fontSize={9}
            fill="hsl(var(--muted-foreground))"
          >
            {`Hop ${layerIdx}`}
          </text>
        );
      })}
    </svg>
  );
}
