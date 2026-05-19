import { useEffect, useRef, useState } from "react";
import { getToken } from "@/lib/auth";

interface StatEvent {
  forward_id: string;
  node_id: string;
  hop_index: number;
  bytes_in: number;
  bytes_out: number;
  active_connections: number;
  ts_unix_ms: number;
}

export interface ForwardRate {
  inRate: number;
  outRate: number;
  activeConnections: number;
}

export function useForwardStats(): Map<string, ForwardRate> {
  const [rates, setRates] = useState<Map<string, ForwardRate>>(new Map());
  // 按 `${forward_id}:${node_id}` 追踪上一次快照，避免不同节点的计数器互相干扰
  const prevRef = useRef<Map<string, StatEvent>>(new Map());
  // 每个入口节点最新的速率（用于多入口节点的汇总）
  const nodeRatesRef = useRef<Map<string, ForwardRate>>(new Map());

  useEffect(() => {
    const token = getToken();
    if (!token) return;

    const es = new EventSource(
      `/api/v1/events/forwards?token=${encodeURIComponent(token)}`,
    );

    es.onmessage = (e) => {
      let evt: StatEvent;
      try {
        evt = JSON.parse(e.data);
      } catch {
        return;
      }

      // 只用入口跳（hop_index=0）做速率计算；其他跳的计数器与入口节点完全独立，
      // 混用会导致巨大的虚假速率（TB/s 级别的显示 bug）
      if (evt.hop_index !== 0) return;

      const nodeKey = `${evt.forward_id}:${evt.node_id}`;
      const prev = prevRef.current.get(nodeKey);
      prevRef.current.set(nodeKey, evt);

      if (!prev) return;

      const dtSec = (evt.ts_unix_ms - prev.ts_unix_ms) / 1000;
      if (dtSec <= 0) return;

      const inRate  = Math.max(0, (evt.bytes_in  - prev.bytes_in)  / dtSec);
      const outRate = Math.max(0, (evt.bytes_out - prev.bytes_out) / dtSec);

      nodeRatesRef.current.set(nodeKey, {
        inRate,
        outRate,
        activeConnections: evt.active_connections,
      });

      // 汇总该 forward 所有入口节点的速率
      const fwdPrefix = `${evt.forward_id}:`;
      let totalIn = 0, totalOut = 0, totalConns = 0;
      for (const [k, r] of nodeRatesRef.current) {
        if (k.startsWith(fwdPrefix)) {
          totalIn   += r.inRate;
          totalOut  += r.outRate;
          totalConns += r.activeConnections;
        }
      }

      setRates((cur) => {
        const next = new Map(cur);
        next.set(evt.forward_id, {
          inRate: totalIn,
          outRate: totalOut,
          activeConnections: totalConns,
        });
        return next;
      });
    };

    return () => es.close();
  }, []);

  return rates;
}
