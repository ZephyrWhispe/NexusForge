import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  Input,
  makeStyles,
  Select,
  Table,
  TableBody,
  TableCell,
  TableRow,
  Text,
  tokens,
} from "@fluentui/react-components";
import {
  fileRemoteBrowse,
  fileRemoteDrivers,
  parseAppError,
  type RemoteDriverDto,
  type RemoteEntryDto,
} from "../../ipc/client";
import { reportError } from "../../stores/notifications";
import EmptyState from "../../components/EmptyState";
import InlineError from "../../components/InlineError";

/**
 * 远端浏览（T-B6-10，承重⑮⑯）：驱动行与根目录**只**出自 file_remote_drivers
 * 的 roots——本地盘符表（fileDrivers）是另一张脸，混表即把"未连接的远端"
 * 假称在场。本面只读：跨边界传输入队尚未接线（队列远端执行器归 T-B6-11），
 * 这里不摆"下载"假钮。
 */

const useStyles = makeStyles({
  toolbar: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" },
  muted: { color: tokens.colorNeutralForeground3, fontSize: tokens.fontSizeBase200 },
  tableWrap: {
    flex: 1,
    minHeight: 0,
    overflowY: "auto",
    border: `1px solid ${tokens.colorNeutralStroke1}`,
    borderRadius: tokens.borderRadiusLarge,
    backgroundColor: tokens.colorNeutralBackground1,
  },
  nameCell: { maxWidth: "360px", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" },
  row: { cursor: "default" },
});

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

export default function RemoteBrowser() {
  const styles = useStyles();
  const [drivers, setDrivers] = useState<RemoteDriverDto[]>([]);
  const [driverId, setDriverId] = useState<string>("");
  const [path, setPath] = useState<string>("");
  const [entries, setEntries] = useState<RemoteEntryDto[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshDrivers = useCallback(async () => {
    try {
      const list = await fileRemoteDrivers();
      setDrivers(list);
      // 已选驱动被断开后不再在场：选择态跟随事实源收敛，不养幽灵选项
      setDriverId((cur) => (list.some((d) => d.driver_id === cur) ? cur : (list[0]?.driver_id ?? "")));
    } catch (e) {
      const ae = parseAppError(e);
      if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
      else {
        reportError(e, { context: "远端驱动列表异常" });
        setError("远端驱动列表读取失败（非典形错误，已上报宿主日志）");
      }
    }
  }, []);

  useEffect(() => {
    void refreshDrivers();
  }, [refreshDrivers]);

  const driver = drivers.find((d) => d.driver_id === driverId) ?? null;

  const browse = useCallback(
    async (target: string) => {
      if (!driver) return;
      setLoading(true);
      setError(null);
      setPath(target);
      try {
        setEntries(await fileRemoteBrowse(driver.driver_id, target));
      } catch (e) {
        setEntries(null);
        const ae = parseAppError(e);
        if (ae) setError(`${ae.data.code}: ${ae.data.message}`);
        else {
          reportError(e, { context: "远端目录浏览异常" });
          setError("远端目录读取失败（非典形错误，已上报宿主日志）");
        }
      } finally {
        setLoading(false);
      }
    },
    [driver],
  );

  // 换驱动即清列表：上一驱动的条目留在屏上就是对另一驱动撒谎
  useEffect(() => {
    setEntries(null);
    setPath(driver ? driver.roots[0] ?? "" : "");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [driverId]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "8px", minHeight: 0, flex: 1 }}>
      <div className={styles.toolbar}>
        <Text weight="semibold" size={300}>
          远端浏览
        </Text>
        <Select
          size="small"
          aria-label="远端驱动"
          value={driverId}
          onChange={(_, d) => setDriverId(d.value)}
          style={{ minWidth: "180px" }}
        >
          {drivers.length === 0 && <option value="">（尚无已连接远端）</option>}
          {drivers.map((d) => (
            <option key={d.driver_id} value={d.driver_id}>
              {d.label}
            </option>
          ))}
        </Select>
        {driver && (
          <Select
            size="small"
            aria-label="根目录"
            value={driver.roots.includes(path) ? path : driver.roots[0] ?? ""}
            onChange={(_, d) => void browse(d.value)}
            style={{ minWidth: "120px" }}
          >
            {/* 根目录只出自 file_remote_drivers.roots（承重⑮）：本地盘符禁入本表 */}
            {driver.roots.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </Select>
        )}
        <Input
          size="small"
          aria-label="远端路径"
          value={path}
          onChange={(_, d) => setPath(d.value)}
          onKeyDown={(ev) => ev.key === "Enter" && path && void browse(path)}
          style={{ maxWidth: "240px" }}
        />
        <Button size="small" disabled={!driver || loading} onClick={() => void browse(path)}>
          浏览
        </Button>
        <Button size="small" appearance="subtle" onClick={() => void refreshDrivers()}>
          刷新
        </Button>
        {driver && <Badge appearance="outline">凭据来源 {driver.auth_source}</Badge>}
      </div>
      {loading && (
        <div className={styles.toolbar} role="status">
          <Text className={styles.muted}>远端目录加载中…</Text>
        </div>
      )}
      {error && <InlineError text={error} />}
      {!driver && !loading && (
        <EmptyState text="尚无已连接远端：先在上方『远程连接』完成连接，这里才会列出驱动" />
      )}
      {driver && entries && (
        <div className={styles.tableWrap}>
          <Table>
            <TableBody>
              {entries.map((e) => (
                <TableRow
                  key={e.path}
                  className={styles.row}
                  onDoubleClick={() => e.is_dir && void browse(e.path)}
                >
                  <TableCell className={styles.nameCell}>
                    {e.is_dir ? "📁 " : "📄 "}
                    {e.name}
                  </TableCell>
                  <TableCell>
                    <Text className={styles.muted}>{e.is_dir ? "目录" : fmtSize(e.size)}</Text>
                  </TableCell>
                </TableRow>
              ))}
              {entries.length === 0 && (
                <TableRow>
                  <TableCell>
                    <Text className={styles.muted}>该远端目录为空</Text>
                  </TableCell>
                </TableRow>
              )}
            </TableBody>
          </Table>
        </div>
      )}
    </div>
  );
}
