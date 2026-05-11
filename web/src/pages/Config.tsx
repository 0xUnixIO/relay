import { useState, useEffect, useRef } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { Copy, Check, Play, RefreshCw, FolderSearch, Rocket } from "lucide-react";
import ReactMarkdown from "react-markdown";
import { Api, ApiError, type BackupJob } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";
import { Badge } from "@/components/ui/badge";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";

export default function ConfigPage() {
  const { data: cfg, mutate } = useSWR("system-config", Api.getConfig);
  const { data: sysVersion, mutate: mutateVersion } = useSWR(
    "system-version",
    Api.getSystemVersion,
  );
  const { data: branding, mutate: mutateBranding } = useSWR(
    "system-branding",
    Api.getBranding,
  );
  const { data: r2cfg, mutate: mutateR2 } = useSWR("r2-backup-config", Api.getR2BackupConfig);

  const [enabled, setEnabled] = useState<boolean | undefined>(undefined);
  const [title, setTitle] = useState<string | undefined>(undefined);
  const [content, setContent] = useState<string | undefined>(undefined);
  const [saving, setSaving] = useState(false);
  const [retentionDays, setRetentionDays] = useState<number | undefined>(undefined);
  const [retentionSaving, setRetentionSaving] = useState(false);
  const [channelSaving, setChannelSaving] = useState(false);
  const [copied, setCopied] = useState(false);
  const [masterMirrorUrl, setMasterMirrorUrl] = useState("");
  const [brandDraft, setBrandDraft] = useState<string | undefined>(undefined);
  const [brandSaving, setBrandSaving] = useState(false);

  const [r2Draft, setR2Draft] = useState<{
    account_id: string;
    bucket_name: string;
    access_key_id: string;
    secret_access_key: string;
    path_prefix: string;
    schedule_hours: number;
    keep_count: number;
  } | null>(null);
  const [r2Saving, setR2Saving] = useState(false);
  const [triggering, setTriggering] = useState(false);
  const [filesOpen, setFilesOpen] = useState(false);
  const [backupFiles, setBackupFiles] = useState<BackupJob[] | null>(null);
  const [loadingFiles, setLoadingFiles] = useState(false);
  const [confirmKey, setConfirmKey] = useState<string | null>(null);
  const [restoringKey, setRestoringKey] = useState<string | null>(null);
  const [selfUpgrading, setSelfUpgrading] = useState(false);
  const [upgradeConfirmOpen, setUpgradeConfirmOpen] = useState(false);
  const [selfUpgradeTarget, setSelfUpgradeTarget] = useState<"stable" | "rc">("stable");
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    return () => {
      if (pollRef.current !== null) clearInterval(pollRef.current);
    };
  }, []);

  const effectiveEnabled = enabled ?? cfg?.announcement_enabled ?? false;
  const effectiveTitle = title ?? cfg?.announcement_title ?? "";
  const effectiveContent = content ?? cfg?.announcement_content ?? "";

  const save = async () => {
    setSaving(true);
    try {
      await Api.updateConfig({
        announcement_enabled: effectiveEnabled,
        announcement_title: effectiveTitle,
        announcement_content: effectiveContent,
      });
      await mutate();
      setEnabled(undefined);
      setTitle(undefined);
      setContent(undefined);
      toast.success("配置已保存");
    } catch (e: any) {
      toast.error(e?.message ?? "保存失败");
    } finally {
      setSaving(false);
    }
  };

  const effectiveRetentionDays = retentionDays ?? cfg?.monitor_retention_days ?? 30;
  const saveRetention = async () => {
    const days = Math.max(1, Math.min(3650, Math.floor(effectiveRetentionDays)));
    setRetentionSaving(true);
    try {
      await Api.updateConfig({ monitor_retention_days: days });
      await mutate();
      setRetentionDays(undefined);
      toast.success("监控数据保留策略已保存");
    } catch (e: any) {
      toast.error(e?.message ?? "保存失败");
    } finally {
      setRetentionSaving(false);
    }
  };

  const setChannel = async (next: "stable" | "rc") => {
    if (sysVersion?.channel === next || channelSaving) return;
    setChannelSaving(true);
    try {
      await Api.setUpgradeChannel(next);
      await mutateVersion();
      toast.success(`已切换升级通道：${next}`);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setChannelSaving(false);
    }
  };

  const masterVersion = sysVersion?.master_version ?? "—";
  const masterTagged = masterVersion.startsWith("v") ? masterVersion : `v${masterVersion}`;
  const upgradeTarget = sysVersion?.latest_stable?.tag ?? masterTagged;
  const masterUpgradeCmd = () => {
    const scriptUrl = `${masterMirrorUrl}https://raw.githubusercontent.com/0xUnixIO/relay/main/install.sh`;
    return `curl -fsSL ${scriptUrl} | sudo bash -s -- --update --version ${upgradeTarget}`;
  };
  const copyCmd = async () => {
    try {
      await navigator.clipboard.writeText(masterUpgradeCmd());
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("复制失败");
    }
  };

  const doSelfUpgrade = async () => {
    setUpgradeConfirmOpen(false);
    setSelfUpgrading(true);
    try {
      await Api.selfUpgradeMaster(selfUpgradeTarget);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
      setSelfUpgrading(false);
      return;
    }
    // Master is restarting — 等 15s 让 updater 把旧进程停掉再开始轮询
    setTimeout(() => {
      pollRef.current = setInterval(async () => {
        try {
          const r = await fetch("/health", { cache: "no-store" });
          if (r.ok) {
            clearInterval(pollRef.current!);
            pollRef.current = null;
            window.location.reload();
          }
        } catch {
          // still restarting
        }
      }, 2000);
    }, 15_000);
  };

  const effectiveBrand = brandDraft ?? branding?.brand_name ?? "RELAY";
  const brandDirty = brandDraft !== undefined && brandDraft.trim() !== (branding?.brand_name ?? "");
  const saveBrand = async () => {
    const next = effectiveBrand.trim();
    if (!next || next === (branding?.brand_name ?? "")) return;
    setBrandSaving(true);
    try {
      await Api.setBranding(next);
      await mutateBranding();
      setBrandDraft(undefined);
      toast.success("品牌名称已保存（刷新页面生效）");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setBrandSaving(false);
    }
  };

  const r2Fields = r2Draft ?? {
    account_id: r2cfg?.account_id ?? "",
    bucket_name: r2cfg?.bucket_name ?? "",
    access_key_id: r2cfg?.access_key_id ?? "",
    secret_access_key: r2cfg?.secret_access_key ?? "",
    path_prefix: r2cfg?.path_prefix ?? "",
    schedule_hours: r2cfg?.schedule_hours ?? 0,
    keep_count: r2cfg?.keep_count ?? 0,
  };
  const saveR2 = async () => {
    setR2Saving(true);
    try {
      await Api.setR2BackupConfig({
        account_id: r2Fields.account_id,
        bucket_name: r2Fields.bucket_name,
        access_key_id: r2Fields.access_key_id,
        secret_access_key: r2Fields.secret_access_key || undefined,
        path_prefix: r2Fields.path_prefix || undefined,
        schedule_hours: r2Fields.schedule_hours,
        keep_count: r2Fields.keep_count,
      });
      await mutateR2();
      setR2Draft(null);
      toast.success("R2 备份配置已保存");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setR2Saving(false);
    }
  };

  const triggerNow = async () => {
    setTriggering(true);
    try {
      await Api.triggerBackup();
      toast.success("备份任务已提交，正在后台执行");
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setTriggering(false);
    }
  };

  const openFilesDialog = async () => {
    setFilesOpen(true);
    setBackupFiles(null);
    setConfirmKey(null);
    setLoadingFiles(true);
    try {
      const files = await Api.listBackupFiles();
      setBackupFiles(files);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
      setFilesOpen(false);
    } finally {
      setLoadingFiles(false);
    }
  };

  const doRestore = async (objectKey: string) => {
    setRestoringKey(objectKey);
    try {
      await Api.restoreBackup(objectKey);
      toast.success("恢复完成，建议刷新页面");
      setConfirmKey(null);
      setFilesOpen(false);
    } catch (e) {
      toast.error(e instanceof ApiError ? e.message : String(e));
    } finally {
      setRestoringKey(null);
    }
  };

  const fmtSize = (bytes: number | null) => {
    if (bytes === null) return "—";
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
  };

  const upgradeTargetTag = selfUpgradeTarget === "stable"
    ? sysVersion?.latest_stable?.tag
    : sysVersion?.latest_rc?.tag;

  return (
    <>
    {/* 重连遮罩 */}
    {selfUpgrading && (
      <div className="fixed inset-0 z-50 flex flex-col items-center justify-center bg-background/90 backdrop-blur-sm">
        <RefreshCw className="h-10 w-10 animate-spin text-primary mb-4" />
        <p className="text-lg font-medium">Master 正在重启…</p>
        <p className="text-sm text-muted-foreground mt-1">升级完成后页面将自动刷新</p>
      </div>
    )}
    {/* 升级确认对话框 */}
    <Dialog open={upgradeConfirmOpen} onOpenChange={setUpgradeConfirmOpen}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>确认升级 Master</DialogTitle>
          <DialogDescription>
            将升级到 <span className="font-mono font-medium">{upgradeTargetTag}</span>，
            Master 服务将重启约 30 秒，期间面板不可用。
          </DialogDescription>
        </DialogHeader>
        <div className="flex justify-end gap-2 mt-2">
          <Button variant="outline" onClick={() => setUpgradeConfirmOpen(false)}>取消</Button>
          <Button onClick={doSelfUpgrade}>确认升级</Button>
        </div>
      </DialogContent>
    </Dialog>
    <div className="space-y-6 max-w-2xl">
      <div>
        <h1 className="text-xl font-semibold">系统配置</h1>
        <p className="text-sm text-muted-foreground mt-1">全局设置，对所有用户生效。</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">品牌名称</CardTitle>
          <CardDescription>
            显示在侧边栏左上角与登录页。最长 32 个字符，会自动以大写形式展示。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex gap-2">
            <Input
              placeholder="RELAY"
              value={effectiveBrand}
              maxLength={32}
              onChange={(e) => setBrandDraft(e.target.value)}
            />
            <Button onClick={saveBrand} disabled={!brandDirty || brandSaving}>
              {brandSaving ? "保存中…" : "保存"}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">公告弹窗</CardTitle>
          <CardDescription>启用后，所有登录用户首次打开面板时将看到此公告。</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="flex items-center gap-3">
            <Switch
              id="ann-enabled"
              checked={effectiveEnabled}
              onCheckedChange={setEnabled}
            />
            <Label htmlFor="ann-enabled">启用公告</Label>
          </div>

          <div className="space-y-1.5">
            <Label>标题</Label>
            <Input
              placeholder="公告标题"
              value={effectiveTitle}
              onChange={(e) => setTitle(e.target.value)}
              disabled={!effectiveEnabled}
            />
          </div>

          <div className="space-y-1.5">
            <Label>内容</Label>
            <Textarea
              placeholder="公告内容（支持 Markdown）"
              value={effectiveContent}
              onChange={(e) => setContent(e.target.value)}
              disabled={!effectiveEnabled}
              rows={5}
            />
            {effectiveContent && (
              <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm text-amber-900 dark:border-amber-800/30 dark:bg-amber-950/30 dark:text-amber-200">
                <p className="text-xs text-amber-600 dark:text-amber-400 mb-1.5 font-medium">预览</p>
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
                  {effectiveContent}
                </ReactMarkdown>
              </div>
            )}
          </div>

          <Button onClick={save} disabled={saving}>
            {saving ? "保存中…" : "保存"}
          </Button>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">监控数据保留</CardTitle>
          <CardDescription>
            forward_stats 和 node_heartbeats 的保留天数（1–3650）。超出时限的历史数据每小时清理一次。
          </CardDescription>
        </CardHeader>
        <CardContent>
          <div className="flex items-center gap-3">
            <Input
              type="number"
              min={1}
              max={3650}
              className="w-32"
              value={effectiveRetentionDays}
              onChange={(e) => {
                const v = Math.max(1, Math.min(3650, Math.floor(Number(e.target.value) || 30)));
                setRetentionDays(v);
              }}
            />
            <span className="text-sm text-muted-foreground">天</span>
            <Button
              onClick={saveRetention}
              disabled={retentionSaving || retentionDays === undefined}
            >
              {retentionSaving ? "保存中…" : "保存"}
            </Button>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">升级通道</CardTitle>
          <CardDescription>
            决定 UI 上「跟随通道」升级节点时使用 stable 还是 rc 的最新版。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="flex gap-2">
            {(["stable", "rc"] as const).map((c) => {
              const active = sysVersion?.channel === c;
              return (
                <button
                  key={c}
                  type="button"
                  onClick={() => setChannel(c)}
                  disabled={channelSaving}
                  className={`flex-1 rounded-md border px-3 py-2 text-sm text-left ${
                    active
                      ? "border-primary bg-primary/5 text-foreground"
                      : "border-border text-muted-foreground hover:border-foreground/30"
                  } disabled:opacity-50`}
                >
                  <div className="font-medium">{c === "stable" ? "稳定版" : "预发布"}</div>
                  <div className="text-xs">
                    {c === "stable"
                      ? sysVersion?.latest_stable?.tag ?? "（暂无）"
                      : sysVersion?.latest_rc?.tag ?? "（暂无）"}
                  </div>
                </button>
              );
            })}
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base flex items-center gap-2">
            Master 版本
            <Badge variant="outline" className="font-mono text-xs">
              {masterVersion}
            </Badge>
          </CardTitle>
          <CardDescription>
            一键升级或手动 SSH 执行命令。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          <div className="space-y-1">
            <Input
              placeholder="GitHub 镜像前缀（可选，如 https://gh-proxy.com/）"
              value={masterMirrorUrl}
              onChange={(e) => setMasterMirrorUrl(e.target.value)}
              className="h-8 text-xs"
            />
            <div className="flex flex-wrap gap-1">
              {([
                { label: "gh-proxy.com", url: "https://gh-proxy.com/" },
                { label: "ghfast.top", url: "https://ghfast.top/" },
                { label: "moeyy.xyz", url: "https://github.moeyy.xyz/" },
              ]).map((preset) => (
                <button
                  key={preset.url}
                  type="button"
                  onClick={() => setMasterMirrorUrl(masterMirrorUrl === preset.url ? "" : preset.url)}
                  className={`px-2 py-0.5 rounded text-xs border transition-colors ${
                    masterMirrorUrl === preset.url
                      ? "bg-primary text-primary-foreground border-primary"
                      : "bg-muted text-muted-foreground border-border hover:bg-accent hover:text-accent-foreground"
                  }`}
                >
                  {preset.label}
                </button>
              ))}
            </div>
          </div>
          <div className="flex gap-2 items-stretch">
            <code className="flex-1 rounded-md border bg-muted/30 px-3 py-2 text-xs font-mono break-all">
              {masterUpgradeCmd()}
            </code>
            <Button variant="outline" size="sm" onClick={copyCmd} className="flex-shrink-0">
              {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
            </Button>
          </div>
          {sysVersion?.latest_stable?.tag && (
            <p className="text-xs text-muted-foreground">
              最新稳定版：{sysVersion.latest_stable.tag}
              {sysVersion?.latest_rc?.tag && <> · 最新预发布：{sysVersion.latest_rc.tag}</>}
            </p>
          )}
          <div className="flex gap-2">
            {(["stable", "rc"] as const).map((ch) => {
              const tag = ch === "stable" ? sysVersion?.latest_stable?.tag : sysVersion?.latest_rc?.tag;
              if (!tag) return null;
              return (
                <Button
                  key={ch}
                  size="sm"
                  variant="outline"
                  onClick={() => { setSelfUpgradeTarget(ch); setUpgradeConfirmOpen(true); }}
                >
                  <Rocket className="h-3.5 w-3.5 mr-1.5" />
                  升级到 {tag}
                </Button>
              );
            })}
          </div>
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle className="text-base flex items-center gap-2">
            备份存储（Cloudflare R2）
            {r2cfg?.configured ? (
              <Badge variant="outline" className="text-xs text-green-600 border-green-300">已配置</Badge>
            ) : (
              <Badge variant="outline" className="text-xs text-muted-foreground">未配置</Badge>
            )}
          </CardTitle>
          <CardDescription>
            配置 Cloudflare R2 作为数据库备份的存储目标。密钥保存后不再回显。
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="grid grid-cols-2 gap-3">
            <div className="space-y-1.5">
              <Label>Account ID</Label>
              <Input
                placeholder="Cloudflare Account ID"
                value={r2Fields.account_id}
                onChange={(e) => setR2Draft({ ...r2Fields, account_id: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <Label>Bucket 名称</Label>
              <Input
                placeholder="my-backup-bucket"
                value={r2Fields.bucket_name}
                onChange={(e) => setR2Draft({ ...r2Fields, bucket_name: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <Label>Access Key ID</Label>
              <Input
                placeholder="R2 Access Key ID"
                value={r2Fields.access_key_id}
                onChange={(e) => setR2Draft({ ...r2Fields, access_key_id: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <Label>Secret Access Key</Label>
              <Input
                placeholder="R2 Secret Access Key"
                value={r2Fields.secret_access_key}
                onChange={(e) => setR2Draft({ ...r2Fields, secret_access_key: e.target.value })}
              />
            </div>
          </div>

          <div className="grid grid-cols-3 gap-3">
            <div className="space-y-1.5">
              <Label>路径前缀 <span className="text-muted-foreground text-xs">（可选）</span></Label>
              <Input
                placeholder="backups/"
                value={r2Fields.path_prefix}
                onChange={(e) => setR2Draft({ ...r2Fields, path_prefix: e.target.value })}
              />
            </div>
            <div className="space-y-1.5">
              <Label>定时间隔（小时，0 禁用）</Label>
              <Input
                type="number"
                min={0}
                placeholder="0"
                value={r2Fields.schedule_hours === 0 ? "" : r2Fields.schedule_hours}
                onChange={(e) => {
                  const v = Math.max(0, Math.floor(Number(e.target.value) || 0));
                  setR2Draft({ ...r2Fields, schedule_hours: v });
                }}
              />
            </div>
            <div className="space-y-1.5">
              <Label>保留份数（0 不限制）</Label>
              <Input
                type="number"
                min={0}
                placeholder="0"
                value={r2Fields.keep_count === 0 ? "" : r2Fields.keep_count}
                onChange={(e) => {
                  const v = Math.max(0, Math.floor(Number(e.target.value) || 0));
                  setR2Draft({ ...r2Fields, keep_count: v });
                }}
              />
            </div>
          </div>

          <div className="flex gap-2">
            <Button onClick={saveR2} disabled={r2Saving}>
              {r2Saving ? "保存中…" : "保存配置"}
            </Button>
            <Button
              variant="outline"
              onClick={triggerNow}
              disabled={triggering || !r2cfg?.configured}
            >
              {triggering
                ? <><RefreshCw className="h-4 w-4 mr-1.5 animate-spin" />备份中…</>
                : <><Play className="h-4 w-4 mr-1.5" />立即备份</>
              }
            </Button>
            <Button
              variant="outline"
              onClick={openFilesDialog}
              disabled={!r2cfg?.configured}
            >
              <FolderSearch className="h-4 w-4 mr-1.5" />
              查询备份文件
            </Button>
          </div>

          <Dialog open={filesOpen} onOpenChange={setFilesOpen}>
            <DialogContent className="max-w-2xl">
              <DialogHeader>
                <DialogTitle>备份文件列表</DialogTitle>
                <DialogDescription>
                  选择一个备份文件进行恢复。恢复操作会覆盖当前全部数据，请谨慎操作。
                </DialogDescription>
              </DialogHeader>
              {loadingFiles ? (
                <div className="flex items-center justify-center py-8 text-sm text-muted-foreground">
                  <RefreshCw className="h-4 w-4 mr-2 animate-spin" />
                  加载中…
                </div>
              ) : backupFiles && backupFiles.length === 0 ? (
                <p className="text-sm text-muted-foreground py-4 text-center">暂无备份文件</p>
              ) : backupFiles && (
                <div className="rounded-md border overflow-hidden text-xs">
                  <table className="w-full">
                    <thead>
                      <tr className="border-b bg-muted/40">
                        <th className="px-3 py-2 text-left font-medium text-muted-foreground">时间</th>
                        <th className="px-3 py-2 text-left font-medium text-muted-foreground">大小</th>
                        <th className="px-3 py-2 text-left font-medium text-muted-foreground">对象键</th>
                        <th className="px-3 py-2 text-left font-medium text-muted-foreground">操作</th>
                      </tr>
                    </thead>
                    <tbody>
                      {backupFiles.map((file) => (
                        <tr key={file.id} className="border-b last:border-0 hover:bg-muted/20">
                          <td className="px-3 py-2 text-muted-foreground whitespace-nowrap">
                            {new Date(file.started_at).toLocaleString("zh-CN", { hour12: false })}
                          </td>
                          <td className="px-3 py-2 tabular-nums">{fmtSize(file.size_bytes)}</td>
                          <td className="px-3 py-2 font-mono text-muted-foreground max-w-[200px] truncate" title={file.object_key ?? ""}>
                            {file.object_key ?? "—"}
                          </td>
                          <td className="px-3 py-2">
                            {confirmKey === file.object_key ? (
                              <div className="flex gap-1">
                                <Button
                                  size="sm"
                                  variant="destructive"
                                  className="h-6 text-xs px-2"
                                  disabled={restoringKey === file.object_key}
                                  onClick={() => doRestore(file.object_key!)}
                                >
                                  {restoringKey === file.object_key ? <RefreshCw className="h-3 w-3 animate-spin" /> : "确认恢复"}
                                </Button>
                                <Button
                                  size="sm"
                                  variant="ghost"
                                  className="h-6 text-xs px-2"
                                  disabled={!!restoringKey}
                                  onClick={() => setConfirmKey(null)}
                                >
                                  取消
                                </Button>
                              </div>
                            ) : (
                              <Button
                                size="sm"
                                variant="outline"
                                className="h-6 text-xs px-2"
                                disabled={!!restoringKey}
                                onClick={() => setConfirmKey(file.object_key!)}
                              >
                                恢复
                              </Button>
                            )}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}
            </DialogContent>
          </Dialog>
        </CardContent>
      </Card>
    </div>
    </>
  );
}
