import useSWR from "swr";
import ReactMarkdown from "react-markdown";
import { Api } from "@/lib/api";
import { Network, Megaphone } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { ProtocolSetBadge } from "@/components/ui/protocol-set-badge";
import { EmptyState } from "@/components/ui/empty-state";

function fmtBytes(n: number): string {
  const u = ["B", "KB", "MB", "GB", "TB"];
  let v = n, i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return `${v.toFixed(v < 10 ? 1 : 0)} ${u[i]}`;
}

export default function UserDashboard() {
  const { data: forwards = [] } = useSWR("forwards", Api.listForwards);
  const { data: cfg } = useSWR("system-config", Api.getConfig, { revalidateOnFocus: false });
  const { data: me } = useSWR("me", Api.getMe);

  const enabled = forwards.filter((f) => f.effective_enabled);
  const totalIn = forwards.reduce((s, f) => s + f.in_flow_bytes, 0);
  const totalOut = forwards.reduce((s, f) => s + f.out_flow_bytes, 0);
  const totalConns = forwards.reduce((s, f) => s + f.active_connections, 0);

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-semibold">首页</h1>

      {cfg?.announcement_enabled && (cfg.announcement_title || cfg.announcement_content) && (
        <div className="rounded-lg border border-amber-200 bg-amber-50 p-4 space-y-2 dark:border-amber-800/30 dark:bg-amber-950/30">
          {cfg.announcement_title && (
            <div className="flex items-center gap-2">
              <Megaphone className="h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400" />
              <span className="font-semibold text-base text-amber-900 dark:text-amber-200">{cfg.announcement_title}</span>
            </div>
          )}
          {cfg.announcement_content && (
            <div className="text-sm text-amber-800/80 dark:text-amber-300/70">
              <ReactMarkdown components={{
                p: ({ children }) => <p className="mb-1 last:mb-0">{children}</p>,
                strong: ({ children }) => <strong className="font-semibold">{children}</strong>,
                em: ({ children }) => <em className="italic">{children}</em>,
                a: ({ href, children }) => (
                  <a href={href} target="_blank" rel="noopener noreferrer" className="underline underline-offset-2">
                    {children}
                  </a>
                ),
                ul: ({ children }) => <ul className="list-disc ml-4 space-y-0.5 mb-1">{children}</ul>,
                ol: ({ children }) => <ol className="list-decimal ml-4 space-y-0.5 mb-1">{children}</ol>,
                h1: ({ children }) => <p className="font-semibold mb-1">{children}</p>,
                h2: ({ children }) => <p className="font-semibold mb-1">{children}</p>,
                h3: ({ children }) => <p className="font-medium mb-1">{children}</p>,
                code: ({ children }) => <code className="font-mono text-xs bg-amber-100 dark:bg-amber-900/40 px-1 rounded">{children}</code>,
              }}>
                {cfg.announcement_content}
              </ReactMarkdown>
            </div>
          )}
        </div>
      )}

      <div className="grid grid-cols-2 gap-4 sm:grid-cols-3">
        {me?.group_name && (
          <div className="rounded-lg border p-4 col-span-2 sm:col-span-3">
            <div className="text-xs text-muted-foreground mb-2">套餐 · {me.group_name}</div>
            {/* 套餐属性：移动端 2 列，sm 以上 4 列，每项用 flex-col 避免长内容截断 */}
            <div className="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-2 text-sm">
              <div className="flex flex-col gap-0.5">
                <span className="text-xs text-muted-foreground">流量</span>
                <span className="font-medium">
                  {me.flow_limit_bytes > 0
                    ? `${(me.flow_limit_bytes / 1_073_741_824).toFixed(0)} GB`
                    : "不限"}
                </span>
              </div>
              <div className="flex flex-col gap-0.5">
                <span className="text-xs text-muted-foreground">限速</span>
                <span className="font-medium">
                  {me.speed_limit_kbps > 0
                    ? `${(me.speed_limit_kbps / 125).toFixed(0)} Mbps`
                    : "不限"}
                </span>
              </div>
              <div className="flex flex-col gap-0.5">
                <span className="text-xs text-muted-foreground">转发上限</span>
                <span className="font-medium">
                  {me.forward_limit > 0 ? `${me.forward_limit} 条` : "不限"}
                </span>
              </div>
              <div className="flex flex-col gap-0.5">
                <span className="text-xs text-muted-foreground">到期</span>
                <span className="font-medium">
                  {me.expires_at ? me.expires_at.slice(0, 10) : "永久"}
                </span>
              </div>
            </div>
          </div>
        )}
        <div className="rounded-lg border p-4 space-y-1">
          <div className="text-sm text-muted-foreground">我的转发</div>
          <div className="text-2xl font-semibold tabular-nums">{enabled.length} / {forwards.length}</div>
        </div>
        <div className="rounded-lg border p-4 space-y-1">
          <div className="text-sm text-muted-foreground">活跃连接</div>
          <div className="text-2xl font-semibold tabular-nums">{totalConns}</div>
        </div>
        <div className="rounded-lg border p-4 space-y-2 col-span-2 sm:col-span-1">
          <div className="text-sm text-muted-foreground">总流量</div>
          <div className="space-y-0.5 text-sm tabular-nums">
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">↓ 下载</span>
              <span className="font-semibold">{fmtBytes(totalIn)}</span>
            </div>
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">↑ 上传</span>
              <span className="font-semibold">{fmtBytes(totalOut)}</span>
            </div>
          </div>
        </div>
      </div>

      <div className="space-y-3">
        <h2 className="text-sm font-medium text-muted-foreground uppercase tracking-wide">我的转发</h2>
        {forwards.length === 0 ? (
          <EmptyState icon={Network} title="暂无转发" description="联系管理员创建转发后会显示在这里。" />
        ) : (
          <div className="rounded-lg border divide-y">
            {forwards.map((f) => (
              <div key={f.id} className="flex items-center justify-between px-4 py-3 text-sm gap-3">
                {/* 左侧：协议徽章 + 名称 */}
                <div className="flex items-center gap-3 min-w-0">
                  <ProtocolSetBadge protocols={f.protocols} />
                  <span className="font-medium truncate">{f.name}</span>
                </div>
                {/* 右侧：桌面端显示流量统计 + Badge；移动端仅 Badge，流量作为次要信息追加在下方 */}
                <div className="flex flex-col items-end gap-1 shrink-0">
                  <Badge variant={f.effective_enabled ? "success" : "outline"}>
                    {f.effective_enabled ? "运行中" : "已停用"}
                  </Badge>
                  <span className="text-xs text-muted-foreground tabular-nums">
                    {f.active_connections} 连接 · ↓{fmtBytes(f.in_flow_bytes)} ↑{fmtBytes(f.out_flow_bytes)}
                  </span>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
