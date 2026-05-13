import { useEffect, useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { Loader2, ArrowUpCircle, AlertCircle, CheckCircle2 } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Api, ApiError, type NodeInfo, type UpgradeJob } from "@/lib/api";

type Mode = "channel" | "tag";

const STATE_LABEL: Record<UpgradeJob["state"], string> = {
  queued: "排队中",
  dispatched: "已下发",
  accepted: "节点已接受",
  succeeded: "升级成功",
  failed: "失败",
  timed_out: "超时",
};

function StateBadge({ state }: { state: UpgradeJob["state"] }) {
  switch (state) {
    case "succeeded":
      return <Badge variant="success">{STATE_LABEL[state]}</Badge>;
    case "failed":
    case "timed_out":
      return <Badge variant="destructive">{STATE_LABEL[state]}</Badge>;
    case "queued":
    case "dispatched":
    case "accepted":
      return <Badge variant="outline">{STATE_LABEL[state]}</Badge>;
  }
}

export function UpgradeNodeDialog({
  open,
  onOpenChange,
  node,
  onSuccess,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  node: NodeInfo;
  onSuccess?: () => void;
}) {
  const supportsUpgrade =
    Array.isArray(node.capabilities) && node.capabilities.includes("upgrade_v1");

  const { data: sysVersion, isLoading: vLoading } = useSWR(
    open ? "system-version" : null,
    Api.getSystemVersion,
  );

  const [mode, setMode] = useState<Mode>("channel");
  const [tagInput, setTagInput] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [activeJobId, setActiveJobId] = useState<number | null>(null);

  // Reset form whenever the dialog reopens.
  useEffect(() => {
    if (open) {
      setMode("channel");
      setTagInput("");
      setActiveJobId(null);
    }
  }, [open]);

  const channelTarget = sysVersion?.master_version
    ? `v${sysVersion.master_version.replace(/^v/, "")}`
    : null;

  const target =
    mode === "channel"
      ? "stable"
      : tagInput.trim();

  const tagInputValid = /^v\d+\.\d+\.\d+$/.test(tagInput.trim());
  const canSubmit =
    supportsUpgrade &&
    !submitting &&
    activeJobId === null &&
    (mode === "channel" ? true : tagInputValid);

  const submit = async () => {
    setSubmitting(true);
    try {
      const job = await Api.upgradeNode(node.id, target);
      setActiveJobId(job.id);
      toast.success(`已下发升级任务 → ${job.target_tag}`);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setSubmitting(false);
    }
  };

  // Poll the active job every 5s while the dialog is open, but stop once
  // the job has reached a terminal state to avoid pointless API traffic.
  const isTerminal = (s?: UpgradeJob["state"]) =>
    s === "succeeded" || s === "failed" || s === "timed_out";
  const { data: job } = useSWR(
    activeJobId !== null && open ? ["upgrade-job", activeJobId] : null,
    () => Api.getUpgradeJob(activeJobId!),
    {
      refreshInterval: (latest) => (isTerminal(latest?.state) ? 0 : 5000),
    },
  );

  useEffect(() => {
    if (!job) return;
    if (job.state === "succeeded") {
      toast.success(`节点已升级到 ${job.target_tag}`);
      onSuccess?.();
    } else if (job.state === "failed" || job.state === "timed_out") {
      toast.error(`升级失败：${job.error || STATE_LABEL[job.state]}`);
    }
  }, [job?.state, job?.error, job?.target_tag]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>升级节点 {node.hostname || node.id}</DialogTitle>
          <DialogDescription>
            当前版本：{node.version || "—"}
          </DialogDescription>
        </DialogHeader>

        {!supportsUpgrade ? (
          <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:border-amber-800/30 dark:bg-amber-950/30 dark:text-amber-200 flex items-start gap-2">
            <AlertCircle className="h-4 w-4 mt-0.5 flex-shrink-0" />
            <div>
              该节点暂不支持远程升级（缺少 <code>upgrade_v1</code> 能力）。
              请先 SSH 到节点手动升级一次到 0.2.x 之后再试。
            </div>
          </div>
        ) : (
          <div className="space-y-4">
            {/* Mode selector */}
            <div className="flex flex-col gap-2">
              <Label>目标</Label>
              <div className="flex gap-2">
                <button
                  type="button"
                  onClick={() => setMode("channel")}
                  className={`flex-1 rounded-md border px-3 py-2 text-sm text-left ${
                    mode === "channel"
                      ? "border-primary bg-primary/5 text-foreground"
                      : "border-border text-muted-foreground hover:border-foreground/30"
                  }`}
                  disabled={!!activeJobId}
                >
                  <div className="font-medium">最新稳定版</div>
                  <div className="text-xs">
                    {vLoading ? "加载中…" : channelTarget ?? "stable"}
                  </div>
                </button>
                <button
                  type="button"
                  onClick={() => setMode("tag")}
                  className={`flex-1 rounded-md border px-3 py-2 text-sm text-left ${
                    mode === "tag"
                      ? "border-primary bg-primary/5 text-foreground"
                      : "border-border text-muted-foreground hover:border-foreground/30"
                  }`}
                  disabled={!!activeJobId}
                >
                  <div className="font-medium">指定 tag</div>
                  <div className="text-xs">vX.Y.Z</div>
                </button>
              </div>
            </div>

            {mode === "tag" && (
              <div className="space-y-1.5">
                <Label htmlFor="upg-tag">tag</Label>
                <Input
                  id="upg-tag"
                  placeholder="v0.2.0"
                  value={tagInput}
                  onChange={(e) => setTagInput(e.target.value)}
                  disabled={!!activeJobId}
                />
                {tagInput && !tagInputValid && (
                  <p className="text-xs text-destructive">tag 格式不正确</p>
                )}
              </div>
            )}

            {/* Job progress */}
            {job && (
              <div className="rounded-md border bg-muted/30 px-3 py-2 text-sm space-y-1.5">
                <div className="flex items-center justify-between">
                  <span className="text-muted-foreground">任务 #{job.id}</span>
                  <StateBadge state={job.state} />
                </div>
                <div className="text-xs text-muted-foreground">
                  目标：{job.target_tag}
                  {job.from_version && <> · 起始：{job.from_version}</>}
                </div>
                {job.error && (
                  <div className="text-xs text-destructive">错误：{job.error}</div>
                )}
                {(job.state === "queued" ||
                  job.state === "dispatched" ||
                  job.state === "accepted") && (
                  <div className="flex items-center gap-1.5 text-xs text-muted-foreground">
                    <Loader2 className="h-3 w-3 animate-spin" /> 正在等待节点完成升级…
                  </div>
                )}
                {job.state === "succeeded" && (
                  <div className="flex items-center gap-1.5 text-xs text-emerald-600">
                    <CheckCircle2 className="h-3 w-3" /> 已升级到 {job.target_tag}
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            {job && job.state !== "queued" && job.state !== "dispatched" && job.state !== "accepted"
              ? "关闭"
              : "取消"}
          </Button>
          {supportsUpgrade && !activeJobId && (
            <Button onClick={submit} disabled={!canSubmit}>
              {submitting ? (
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              ) : (
                <ArrowUpCircle className="mr-2 h-4 w-4" />
              )}
              下发升级
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
