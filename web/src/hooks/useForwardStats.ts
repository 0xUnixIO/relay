import { useEffect, useRef, useState } from "react";
import { getToken } from "@/lib/auth";

interface StatEvent {
  forward_id: string;
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
  const prevRef = useRef<Map<string, StatEvent>>(new Map());

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

      const prev = prevRef.current.get(evt.forward_id);
      prevRef.current.set(evt.forward_id, evt);

      if (!prev) return;

      const dtSec = (evt.ts_unix_ms - prev.ts_unix_ms) / 1000;
      if (dtSec <= 0) return;

      const inRate = Math.max(0, (evt.bytes_in - prev.bytes_in) / dtSec);
      const outRate = Math.max(0, (evt.bytes_out - prev.bytes_out) / dtSec);

      setRates((cur) => {
        const next = new Map(cur);
        next.set(evt.forward_id, {
          inRate,
          outRate,
          activeConnections: evt.active_connections,
        });
        return next;
      });
    };

    return () => es.close();
  }, []);

  return rates;
}
