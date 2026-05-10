const SCRIPT_URL =
  "https://raw.githubusercontent.com/0xUnixIO/relay/main/install-node.sh";

interface ShellCmdProps {
  setupUrl: string | null;
  mirrorUrl?: string;
}

export function ShellCmd({ setupUrl, mirrorUrl }: ShellCmdProps) {
  if (!setupUrl) {
    return <span className="text-muted-foreground">// 生成安装链接中…</span>;
  }

  const hasMirror = !!mirrorUrl?.trim();

  return (
    <>
      <span className="text-blue-600 dark:text-blue-400">bash</span>
      {" <("}
      <span className="text-blue-600 dark:text-blue-400">curl</span>
      {" -fsSL "}
      <span className="text-emerald-700 dark:text-emerald-400">{SCRIPT_URL}</span>
      {")"} <span className="text-violet-600 dark:text-violet-400">\</span>
      {"\n  "}
      <span className="text-violet-600 dark:text-violet-400">--setup</span>
      {" "}
      <span className="text-emerald-700 dark:text-emerald-400">{setupUrl}</span>
      {hasMirror && (
        <>
          {" "}
          <span className="text-violet-600 dark:text-violet-400">\</span>
          {"\n  "}
          <span className="text-violet-600 dark:text-violet-400">--mirror</span>
          {" "}
          <span className="text-emerald-700 dark:text-emerald-400">{mirrorUrl!.trim()}</span>
        </>
      )}
    </>
  );
}
