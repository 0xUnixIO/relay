import { useEffect, useRef, useState } from "react";
import type { ForwardProbeHop } from "@/lib/api";

// 将 scale 夹在 [min, max] 范围内
function clamp(v: number, min: number, max: number) {
  return Math.min(max, Math.max(min, v));
}

// 根据文本长度估算节点宽度
function nodeWidth(label: string) {
  return Math.max(72, label.length * 7.5 + 20);
}

// 拖拽状态，存放在 ref 中以避免 stale closure
interface DragOrigin {
  ox: number;
  oy: number;
  otx: number;
  oty: number;
}

interface Transform {
  scale: number;
  tx: number;
  ty: number;
}

export function ProbeTopology({
  hops,
  streaming,
}: {
  hops: ForwardProbeHop[];
  streaming?: boolean;
}) {
  const NODE_H = 28;
  const H_GAP = 100;
  const V_GAP = 44;
  const PAD = 16;

  const containerRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<DragOrigin | null>(null);
  const [t, setT] = useState<Transform>({ scale: 1, tx: 0, ty: 0 });
  const fittedRef = useRef(false);
  const svgSizeRef = useRef({ w: 0, h: 0 });

  useEffect(() => {
    if (hops.length === 0) {
      fittedRef.current = false;
      return;
    }
    // 流结束时重置，确保最终布局（所有节点到齐后）再做一次 fit
    if (!streaming) fittedRef.current = false;
    if (fittedRef.current) return;

    const el = containerRef.current;
    if (!el) return;

    const tryFit = () => {
      const { w, h } = svgSizeRef.current;
      const rect = el.getBoundingClientRect();
      if (rect.width === 0 || w === 0 || h === 0) return false;
      fittedRef.current = true;
      const fit = Math.min(rect.width / w, rect.height / h);
      const s = clamp(fit >= 1 ? 1 : fit * 0.9, 0.15, 1);
      setT({ scale: s, tx: (rect.width - w * s) / 2, ty: (rect.height - h * s) / 2 });
      return true;
    };

    if (tryFit()) return;
    const ro = new ResizeObserver(() => { if (tryFit()) ro.disconnect(); });
    ro.observe(el);
    return () => ro.disconnect();
  }, [hops.length, streaming]);

  // 空 hops 且仍在 streaming 时展示等待提示
  if (hops.length === 0) {
    return (
      <div className="flex h-24 items-center justify-center gap-2 text-sm text-muted-foreground">
        <span className="h-4 w-4 rounded-full border-2 border-primary border-t-transparent animate-spin" />
        探测中，等待第一段结果…
      </div>
    );
  }

  // 构建节点标签（不截断）
  const labels = new Map<string, string>();
  for (const h of hops) {
    labels.set(h.from_node, h.from_node_name || h.from_node.slice(0, 8));
    const toId = h.to_node || h.target;
    if (toId) {
      const toLabel =
        h.to_node_name ||
        (h.target ? h.target.split(":")[0] : toId.slice(0, 14));
      labels.set(toId, toLabel);
    }
  }

  const edges = hops.flatMap((h) => {
    const toId = h.to_node || h.target;
    return toId ? [{ from: h.from_node, to: toId, us: h.latency_us, ok: h.ok }] : [];
  });

  // 拓扑分层（BFS / longest-path）
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

  // 每层的节点宽度（取该层最宽标签）
  const layerWidths = new Map<number, number>();
  for (const [l, nodes] of layers) {
    const w = Math.max(...nodes.map((id) => nodeWidth(labels.get(id) ?? id)));
    layerWidths.set(l, w);
  }

  // 各层 x 偏移（累加前置层宽 + H_GAP）
  const layerX: number[] = [];
  let curX = PAD;
  for (let l = 0; l < numLayers; l++) {
    layerX[l] = curX;
    curX += (layerWidths.get(l) ?? 72) + H_GAP;
  }

  const svgW = curX - H_GAP + PAD; // 最后一层右侧 PAD
  const svgH = PAD * 2 + maxPerLayer * NODE_H + (maxPerLayer - 1) * V_GAP;
  svgSizeRef.current = { w: svgW, h: svgH };

  // 计算每个节点中心坐标
  const pos = new Map<string, { x: number; y: number; w: number }>();
  for (const [l, nodes] of layers) {
    const x = layerX[l];
    const w = layerWidths.get(l) ?? 72;
    const totalH = nodes.length * NODE_H + (nodes.length - 1) * V_GAP;
    const startY = PAD + (svgH - PAD * 2 - totalH) / 2;
    nodes.forEach((id, i) =>
      pos.set(id, { x, y: startY + i * (NODE_H + V_GAP), w })
    );
  }

  // 延迟颜色
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
  const totalColor =
    total / 1000 < 80 ? "#059669" : total / 1000 < 200 ? "#d97706" : "#e11d48";

  // 适应容器函数（在 svgW/svgH 计算后定义，直接捕获）
  function fitToContainer() {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const cw = rect.width;
    const ch = rect.height;
    const fit = Math.min(cw / svgW, ch / svgH);
    const s = clamp(fit >= 1 ? 1 : fit * 0.9, 0.15, 1);
    setT({
      scale: s,
      tx: (cw - svgW * s) / 2,
      ty: (ch - svgH * s) / 2,
    });
  }

  // 滚轮缩放（向光标位置缩放）
  function handleWheel(e: React.WheelEvent<HTMLDivElement>) {
    e.preventDefault();
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    setT((prev) => {
      const factor = e.deltaY < 0 ? 1.15 : 1 / 1.15;
      const ns = clamp(prev.scale * factor, 0.15, 4);
      return {
        scale: ns,
        tx: mx - (mx - prev.tx) * (ns / prev.scale),
        ty: my - (my - prev.ty) * (ns / prev.scale),
      };
    });
  }

  // 拖拽开始
  function handleMouseDown(e: React.MouseEvent<HTMLDivElement>) {
    if (e.button !== 0) return;
    dragRef.current = {
      ox: e.clientX,
      oy: e.clientY,
      otx: t.tx,
      oty: t.ty,
    };
  }

  // 拖拽移动
  function handleMouseMove(e: React.MouseEvent<HTMLDivElement>) {
    if (!dragRef.current) return;
    const { ox, oy, otx, oty } = dragRef.current;
    setT((prev) => ({
      ...prev,
      tx: otx + (e.clientX - ox),
      ty: oty + (e.clientY - oy),
    }));
  }

  // 拖拽结束
  function handleMouseUp() {
    dragRef.current = null;
  }

  // 控制按钮：放大/缩小（以容器中心为基准）
  function zoomBy(factor: number) {
    if (!containerRef.current) return;
    const rect = containerRef.current.getBoundingClientRect();
    const mx = rect.width / 2;
    const my = rect.height / 2;
    setT((prev) => {
      const ns = clamp(prev.scale * factor, 0.15, 4);
      return {
        scale: ns,
        tx: mx - (mx - prev.tx) * (ns / prev.scale),
        ty: my - (my - prev.ty) * (ns / prev.scale),
      };
    });
  }

  return (
    <div className="space-y-2">
      {/* 控制栏 */}
      <div className="flex items-center justify-end gap-1">
        <button
          type="button"
          onClick={() => zoomBy(1.2)}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="放大"
        >
          ＋
        </button>
        <button
          type="button"
          onClick={() => zoomBy(1 / 1.2)}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="缩小"
        >
          －
        </button>
        <button
          type="button"
          onClick={fitToContainer}
          className="rounded border px-1.5 py-0.5 text-xs hover:bg-muted"
          title="适应窗口"
        >
          适应
        </button>
        <span className="w-10 text-right font-mono text-xs text-muted-foreground">
          {Math.round(t.scale * 100)}%
        </span>
      </div>

      {/* 可平移/缩放画布容器 */}
      <div
        ref={containerRef}
        style={{ height: 300, overflow: "hidden", cursor: dragRef.current ? "grabbing" : "grab" }}
        onWheel={handleWheel}
        onMouseDown={handleMouseDown}
        onMouseMove={handleMouseMove}
        onMouseUp={handleMouseUp}
        onMouseLeave={handleMouseUp}
      >
        <div
          style={{
            transform: `translate(${t.tx}px, ${t.ty}px) scale(${t.scale})`,
            transformOrigin: "0 0",
            willChange: "transform",
          }}
        >
          <svg
            width={svgW}
            height={svgH}
            style={{ overflow: "visible" }}
          >
            {/* 边 */}
            {edges.map((e, i) => {
              const f = pos.get(e.from);
              const tgt = pos.get(e.to);
              if (!f || !tgt) return null;
              const x1 = f.x + f.w;
              const y1 = f.y + NODE_H / 2;
              const x2 = tgt.x;
              const y2 = tgt.y + NODE_H / 2;
              const mx = (x1 + x2) / 2;
              const color = e.ok ? latColor(e.us) : "#e11d48";
              const midY = (y1 + y2) / 2;
              return (
                <g key={i}>
                  <path
                    d={`M${x1} ${y1} C${mx} ${y1} ${mx} ${y2} ${x2} ${y2}`}
                    fill="none"
                    stroke={color}
                    strokeWidth={1.5}
                    opacity={0.8}
                    strokeDasharray={e.ok ? undefined : "4 2"}
                  />
                  <text
                    x={mx}
                    y={midY - 4}
                    textAnchor="middle"
                    fontSize={9}
                    fill={color}
                    fontFamily="ui-monospace,monospace"
                    fontWeight={600}
                  >
                    {e.ok ? `${(e.us / 1000).toFixed(1)}ms` : "✗"}
                  </text>
                </g>
              );
            })}

            {/* 节点 */}
            {[...pos.entries()].map(([id, { x, y, w }]) => (
              <g key={id}>
                <rect
                  x={x}
                  y={y}
                  width={w}
                  height={NODE_H}
                  rx={6}
                  fill="hsl(var(--muted))"
                  stroke="hsl(var(--border))"
                  strokeWidth={1}
                />
                <text
                  x={x + w / 2}
                  y={y + NODE_H / 2 + 4}
                  textAnchor="middle"
                  fontSize={11}
                  fontWeight={500}
                  fill="hsl(var(--foreground))"
                >
                  {labels.get(id) ?? id}
                </text>
              </g>
            ))}
          </svg>
        </div>
      </div>

      {/* 合计延迟 */}
      {allOk && hops.length > 0 && (
        <div className="flex items-center justify-between border-t pt-2 text-sm">
          <span className="text-muted-foreground">合计</span>
          <span
            className="font-mono tabular-nums font-semibold"
            style={{ color: totalColor }}
          >
            {(total / 1000).toFixed(1)} ms
          </span>
        </div>
      )}

      {/* 流式探测中提示 */}
      {streaming && (
        <div className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
          <span className="h-3 w-3 rounded-full border-2 border-primary border-t-transparent animate-spin" />
          探测中…
        </div>
      )}
    </div>
  );
}
