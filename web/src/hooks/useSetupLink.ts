import { useState } from "react";
import { toast } from "sonner";
import { Api } from "@/lib/api";

export function useSetupLink(enrollEndpoint: string | undefined) {
  const [setupUrl, setSetupUrl] = useState<string | null>(null);
  const [mirrorUrl, setMirrorUrl] = useState("");
  const [cmdCopied, setCmdCopied] = useState(false);

  const generateSetup = async (nodeId: string) => {
    setSetupUrl(null);
    try {
      const resp = await Api.createInstallBundle(nodeId);
      const base = enrollEndpoint?.replace(/\/api\/.*$/, "") ?? "";
      setSetupUrl(`${base}/api/v1/setup/${resp.token}`);
    } catch {
      toast.error("生成安装链接失败，请重试");
    }
  };

  const installCmd = () => {
    if (!setupUrl) return "// 生成安装链接中…";
    const lines = [
      `bash <(curl -fsSL https://raw.githubusercontent.com/0xUnixIO/relay/main/install-node.sh) \\`,
      `  --setup ${setupUrl}`,
    ];
    if (mirrorUrl.trim()) {
      lines[lines.length - 1] += " \\";
      lines.push(`  --mirror ${mirrorUrl.trim()}`);
    }
    return lines.join("\n");
  };

  const copyCmd = () => {
    navigator.clipboard.writeText(installCmd());
    setCmdCopied(true);
    setTimeout(() => setCmdCopied(false), 2000);
  };

  return { setupUrl, mirrorUrl, setMirrorUrl, cmdCopied, generateSetup, installCmd, copyCmd };
}
