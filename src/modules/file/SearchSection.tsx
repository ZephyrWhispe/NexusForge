import { useRef, useState } from "react";
import { Badge, Button, Input, makeStyles, Text, tokens } from "@fluentui/react-components";
import { fileSearch, parseAppError, type SearchResultDto } from "../../ipc/client";
import InlineError from "../../components/InlineError";

/**
 * 搜索档（T-B7-27，panels/04 §2「搜索」行）：查询框+结果表自 FilePanel browse 档
 * 逐行挪来（runSearch/降级横幅/空查询撤下等判据一字未动）；
 * 命中点击仍走父级 openPreview（预览分栏是两档共用的同一事实源，不自建第二份）。
 */

const useStyles = makeStyles({
  toolbar: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  queue: {
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusMedium,
    padding: "8px 12px",
    display: "flex",
    flexDirection: "column",
    gap: "6px",
    backgroundColor: tokens.colorNeutralBackground2,
  },
  hitRow: { display: "flex", alignItems: "center", gap: "8px", minWidth: 0 },
  crumbBtn: { padding: "2px 6px", minWidth: 0 },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  warn: { color: tokens.colorPaletteDarkOrangeForeground1, fontSize: tokens.fontSizeBase200 },
});

export default function SearchSection({
  onOpenPreview,
}: {
  onOpenPreview: (path: string) => void;
}) {
  const styles = useStyles();
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchRes, setSearchRes] = useState<SearchResultDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const searchSeq = useRef(0);

  // 全局搜索（F5）：limit 取默认档位 50；root 传 null → 降级遍历走后端默认用户主目录，
  // 不随当前 cwd（否则站在 C:\ 会把降级遍历扩成整盘扫描）。
  const runSearch = async () => {
    const q = query.trim();
    if (!q) {
      setSearchRes(null);
      return;
    }
    const seq = ++searchSeq.current;
    setSearching(true);
    setSearchRes(null);
    setError(null);
    try {
      const res = await fileSearch(q, 50, null);
      if (seq === searchSeq.current) setSearchRes(res);
    } catch (e) {
      if (seq === searchSeq.current) {
        const err = parseAppError(e);
        setError(err ? `${err.data.code}: ${err.data.message}` : "搜索失败");
      }
    } finally {
      if (seq === searchSeq.current) setSearching(false);
    }
  };

  const onQueryChange = (v: string) => {
    setQuery(v);
    if (!v.trim()) {
      // 清空即作废在途请求并撤下结果区（负例判据：空查询不残留旧命中）
      searchSeq.current += 1;
      setSearching(false);
      setSearchRes(null);
    }
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
      <div className={styles.toolbar}>
        <Input
          size="small"
          placeholder="搜索文件名（全局）"
          value={query}
          onChange={(_, d) => onQueryChange(d.value)}
          onKeyDown={(ev) => ev.key === "Enter" && void runSearch()}
          aria-label="全局搜索关键词"
          style={{ maxWidth: "240px" }}
        />
        <Button size="small" onClick={() => void runSearch()}>
          搜索
        </Button>
        <span style={{ flex: 1 }} />
        <InlineError text={error} />
      </div>

      {/* 搜索结果条（F5）：加载/空查询不残留旧命中；降级必须显式标注 */}
      {searching && (
        <div className={styles.queue} role="status">
          <Text className={styles.muted}>正在全局搜索…</Text>
        </div>
      )}
      {searchRes && !searching && (
        <div className={styles.queue}>
          {searchRes.degraded && (
            <Text className={styles.warn}>索引降级：本次为目录遍历（深度≤6）</Text>
          )}
          {searchRes.hits.length === 0 && (
            <Text className={styles.muted}>
              没有名称匹配「{query.trim()}」的命中
              {searchRes.degraded ? "（降级遍历仅覆盖用户主目录）" : ""}
            </Text>
          )}
          {searchRes.hits.slice(0, 12).map((h) => (
            <div key={h.path} className={styles.hitRow}>
              <Button
                appearance="subtle"
                size="small"
                className={styles.crumbBtn}
                title={h.path}
                onClick={() => onOpenPreview(h.path)}
              >
                {h.path}
              </Button>
              <Badge appearance="outline">{h.score}</Badge>
            </div>
          ))}
          {searchRes.hits.length > 12 && (
            <Text className={styles.muted}>共 {searchRes.hits.length} 条，仅显示前 12 条</Text>
          )}
        </div>
      )}
    </div>
  );
}
