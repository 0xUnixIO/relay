import { useEffect, useState } from "react";
import { getToken } from "@/lib/auth";
import type { NodeSpeed } from "@/lib/api";

export function useNodeSpeeds(): Record<string, NodeSpeed> {
  const [speeds, setSpeeds] = useState<Record<string, NodeSpeed>>({});

  useEffect(() => {
    const token = getToken();
    if (!token) return;

    const es = new EventSource(
      `/api/v1/nodes/speeds/stream?token=${encodeURIComponent(token)}`,
    );

    // 服务端推全量快照（watch 语义），直接替换
    es.onmessage = (e: MessageEvent) => {
      try {
        setSpeeds(JSON.parse(e.data as string) as Record<string, NodeSpeed>);
      } catch {
        // 忽略解析错误
      }
    };

    return () => es.close();
  }, []);

  return speeds;
}
